//! Tests for [`super`]: ranking a session's transcript copies across account
//! roots, and the migration carry that reads from the newest one
//! ([`copy_transcript_among`]). Every root is a temp dir; nothing reads the
//! machine's real account roots.

#![cfg(test)]

use super::super::{
    copy_transcript, copy_transcript_among, copy_transcript_discovering, delta_sync, MainCarry,
    TranscriptCopy,
};
use super::*;
use qontinui_runner_lib::session_archive::discovery::AmbientDiscovery;
use std::time::{Duration, SystemTime};

const WD: &str = "/home/u/proj";
const SID: &str = "4b106ec9-00d5-4b4a-842f-f180183fc288";

/// One transcript turn stamped `ts`.
fn rec(ts: &str, text: &str) -> String {
    format!(r#"{{"type":"user","timestamp":"{ts}","message":{{"content":"{text}"}}}}"#) + "\n"
}

/// One non-turn record of `kind` stamped `ts` (`queue-operation`,
/// `attachment`, …).
fn non_turn(kind: &str, ts: &str) -> String {
    format!(r#"{{"type":"{kind}","timestamp":"{ts}","content":"x"}}"#) + "\n"
}

/// A harness-injected `isMeta` user line stamped `ts`: not a turn.
fn meta_line(ts: &str) -> String {
    format!(r#"{{"type":"user","isMeta":true,"timestamp":"{ts}","message":{{"content":"m"}}}}"#)
        + "\n"
}

/// The two turns every copy below shares: the conversation before any
/// account hop.
fn base() -> String {
    rec("2026-11-14T00:50:00.000Z", "first") + &rec("2026-11-14T00:53:00.000Z", "second")
}

/// [`base`] plus a later turn: a strict byte-extension of it.
fn extended() -> String {
    base() + &rec("2026-11-16T09:00:00.000Z", "the live tail")
}

/// A fork of [`base`] whose last turn is from 11-14. Its tail is LONGER than
/// [`fork_new`]'s, so a size rule would pick it.
fn fork_old() -> String {
    base()
        + &rec(
            "2026-11-14T01:00:00.000Z",
            "a stale fork whose tail is the longer of the two",
        )
}

/// A fork of [`base`] that ran on to 11-16.
fn fork_new() -> String {
    base() + &rec("2026-11-16T09:00:00.000Z", "live")
}

/// A copy with no turn in reach: its `user` lines carry no timestamp.
fn untimed(tag: &str) -> String {
    format!("{{\"type\":\"user\"}}\n{{\"x\":\"{tag}\"}}\n")
}

fn main_path(root: &Path) -> PathBuf {
    crate::terminal::transcript::session_transcript_path(root, WD, SID)
}

fn sidecar(root: &Path) -> PathBuf {
    main_path(root).with_file_name(SID)
}

fn write_main(root: &Path, body: &str) -> PathBuf {
    let path = main_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

fn read_main(root: &Path) -> String {
    std::fs::read_to_string(main_path(root)).unwrap()
}

fn write_side(root: &Path, rel: &str, body: &str) {
    let path = sidecar(root).join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn set_mtime(path: &Path, when: SystemTime) {
    filetime::set_file_mtime(path, filetime::FileTime::from_system_time(when)).unwrap();
}

fn two_days_ago() -> SystemTime {
    SystemTime::now() - Duration::from_secs(2 * 24 * 3600)
}

/// Every `<SID>.jsonl.diverged-*` backup beside `root`'s main transcript.
fn backups(root: &Path) -> Vec<PathBuf> {
    let prefix = format!("{SID}.jsonl.diverged-");
    match std::fs::read_dir(main_path(root).parent().unwrap()) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .map(|e| e.path())
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn carry(roots: &[PathBuf], src: &Path, dst: &Path) -> TranscriptCopy {
    copy_transcript_among(roots, src.to_str().unwrap(), dst.to_str().unwrap(), WD, SID).unwrap()
}

fn agent_ids(carried: &TranscriptCopy) -> Vec<&str> {
    carried
        .subagents
        .iter()
        .map(|s| s.agent_id.as_str())
        .collect()
}

/// Asserts the destination was kept in place by `carried`: nothing carried
/// over it, no backup of it, its bytes unchanged.
fn assert_kept(carried: &TranscriptCopy, dst: &Path, body: &str, case: &str) {
    assert_eq!(carried.main, MainCarry::Present, "{case}");
    assert_eq!(carried.carried_from, dst, "{case}");
    assert_eq!(read_main(dst), body, "{case}: bytes unchanged");
    assert!(backups(dst).is_empty(), "{case}: {:?}", backups(dst));
}

/// Three account roots in roster order: the stale source first (as `~/.claude`
/// sorted first on the incident box), then the destination, then the root
/// holding the live copy.
/// The fully ranked verdict naming another root.
fn other(root: &Path) -> Newest {
    Newest::Other {
        root: root.to_path_buf(),
        unranked: None,
    }
}

fn roots(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        tmp.join(".claude"),
        tmp.join(".claude-carolin"),
        tmp.join(".claude-erik"),
    )
}

/// The incident: the migration's source holds an OLDER copy (a strict prefix)
/// than a third root, where the session really ran. The destination gets the
/// third root's bytes and its `<sid>/` side-car, and `carried_from` names
/// that root. Today's carry copied the source's stale copy.
#[test]
fn a_third_roots_newer_copy_is_carried_instead_of_the_stale_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    write_main(&src, &base());
    write_side(&src, "subagents/agent-stale.jsonl", "stale\n");
    write_main(&third, &extended());
    write_side(&third, "subagents/agent-live.jsonl", "live\n");

    let carried = carry(&[src.clone(), dst.clone(), third.clone()], &src, &dst);

    assert_eq!(read_main(&dst), extended(), "the newest copy is carried");
    assert_eq!(carried.main, MainCarry::Copied);
    assert_eq!(carried.carried_from, third);
    assert_eq!(
        carried.src_session_dir,
        sidecar(&third),
        "delta_sync follows it"
    );
    assert!(sidecar(&dst).join("subagents/agent-live.jsonl").is_file());
    assert!(
        !sidecar(&dst).join("subagents/agent-stale.jsonl").exists(),
        "the stale source's side-car is not carried"
    );
    assert_eq!(agent_ids(&carried), vec!["live"]);
    assert_eq!(read_main(&src), base(), "the source is never touched");
}

/// The same with DIVERGED copies: neither is a prefix of the other, so the
/// newest turn decides. The destination held an older prefix of both and is
/// extended from the third root.
#[test]
fn a_diverged_third_root_with_the_later_turn_is_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    write_main(&src, &fork_old());
    write_main(&dst, &base());
    write_main(&third, &fork_new());

    let carried = carry(&[src.clone(), dst.clone(), third.clone()], &src, &dst);

    assert_eq!(read_main(&dst), fork_new());
    assert_eq!(carried.main, MainCarry::ExtendedPrefix);
    assert_eq!(carried.carried_from, third);
    assert!(backups(&dst).is_empty(), "{:?}", backups(&dst));
}

/// Diverged copies are ranked by their newest TURN, not their last record of
/// any type. The stale fork is the migration's source, and its tail ends in a
/// `queue-operation`, an `isMeta` user line and an `attachment`, all stamped
/// LATER than the live copy's newest turn. It still loses: the live copy in
/// the third root is carried.
#[test]
fn a_stale_fork_whose_tail_ends_in_later_non_turn_records_still_loses() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    let stale = fork_old()
        + &non_turn("queue-operation", "2026-11-16T10:00:00.000Z")
        + &meta_line("2026-11-16T10:01:00.000Z")
        + &non_turn("attachment", "2026-11-16T10:02:00.000Z");
    write_main(&src, &stale);
    write_main(&third, &fork_new());
    let all = [src.clone(), dst.clone(), third.clone()];

    assert_eq!(rank(&src, &dst, &all, WD, SID), other(&third));
    let carried = carry(&all, &src, &dst);
    assert_eq!(carried.carried_from, third);
    assert_eq!(read_main(&dst), fork_new());
}

/// A destination that already holds the newest copy is kept in place, by
/// prefix (it extends the source) and by newest turn (it diverged and ran
/// later). There is no `.diverged-*` backup of it, its bytes and mtime are
/// unchanged, and nothing from the source's `<sid>/` lands on it. Today's
/// carry moved it aside and copied the stale source over it. The roots list
/// is empty, so this also shows the source and destination are ranked
/// whether or not the roster lists them. A later delta sync copies nothing.
#[test]
fn a_destination_that_holds_the_newest_copy_is_kept_in_place() {
    for (case, src_body, dst_body) in [
        ("prefix", base(), extended()),
        ("diverged", fork_old(), fork_new()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst, _) = roots(tmp.path());
        write_main(&src, &src_body);
        write_side(&src, "custom-title.json", r#"{"customTitle":"stale"}"#);
        write_side(&src, "subagents/agent-stale.jsonl", "stale\n");
        let dst_main = write_main(&dst, &dst_body);
        write_side(&dst, "custom-title.json", r#"{"customTitle":"live"}"#);
        write_side(&dst, "subagents/agent-live.jsonl", "live\n");
        set_mtime(&dst_main, two_days_ago());
        let mtime_before = std::fs::metadata(&dst_main).unwrap().modified().unwrap();

        assert_eq!(
            rank(&src, &dst, &[], WD, SID),
            Newest::Destination { unranked: None },
            "{case}"
        );
        let carried = carry(&[], &src, &dst);

        assert_kept(&carried, &dst, &dst_body, case);
        assert_eq!(
            std::fs::metadata(&dst_main).unwrap().modified().unwrap(),
            mtime_before,
            "{case}: not rewritten"
        );
        assert_eq!(
            std::fs::read_to_string(sidecar(&dst).join("custom-title.json")).unwrap(),
            r#"{"customTitle":"live"}"#,
            "{case}"
        );
        assert!(
            !sidecar(&dst).join("subagents/agent-stale.jsonl").exists(),
            "{case}"
        );
        assert_eq!(agent_ids(&carried), vec!["live"], "{case}");
        assert_eq!(carried.src_session_dir, sidecar(&dst), "{case}");
        assert_eq!(carried.dst_session_dir, sidecar(&dst), "{case}");
        assert_eq!(carried.session_dir.copied, 0, "{case}");
        assert_eq!(carried.session_dir.skipped_present, 2, "{case}");

        let synced = delta_sync(&carried).unwrap();
        assert_eq!(synced.copied, 0, "{case}: {synced:?}");
        assert_eq!(synced.unchanged, 1, "{case}: {synced:?}");
        assert_eq!(synced.kept_destination_newer, 1, "{case}: {synced:?}");
        assert_eq!(
            read_main(&src),
            src_body,
            "{case}: the source is never touched"
        );
    }
}

/// When the full ranking is UNKNOWN only because a third root cannot be
/// ranked (a diverged copy with no turn in reach), but the destination is
/// PROVEN to strictly extend the source, the destination is kept rather than
/// the source carried over it.
#[test]
fn an_unrankable_third_root_does_not_bury_a_destination_that_extends_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    write_main(&src, &base());
    write_main(&dst, &extended());
    write_main(&third, &untimed("third"));
    let all = [src.clone(), dst.clone(), third.clone()];

    assert!(
        matches!(
            rank(&src, &dst, &all, WD, SID),
            Newest::Destination { unranked: Some(_) }
        ),
        "{:?}",
        rank(&src, &dst, &all, WD, SID)
    );
    let carried = carry(&all, &src, &dst);
    assert_kept(&carried, &dst, &extended(), "unranked third root");
}

/// A source that holds the newest copy carries exactly as before: from the
/// source, with the same verdict on the destination. The third root's copy
/// is a stale fork that is LARGER and has the FRESHER mtime, so a size or
/// mtime rule would have picked it. One case extends a prefix destination;
/// the other moves a diverged destination aside, as a migration always has.
#[test]
fn a_source_that_holds_the_newest_copy_carries_exactly_as_before() {
    for (case, dst_body) in [
        ("prefix destination", base()),
        ("diverged destination", fork_old()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst, third) = roots(tmp.path());
        let src_main = write_main(&src, &fork_new());
        set_mtime(&src_main, two_days_ago());
        write_main(&dst, &dst_body);
        write_main(&third, &fork_old());
        assert!(fork_old().len() > fork_new().len());

        let carried = carry(&[third.clone(), dst.clone(), src.clone()], &src, &dst);

        assert_eq!(carried.carried_from, src, "{case}");
        assert_eq!(read_main(&dst), fork_new(), "{case}");
        match case {
            "prefix destination" => {
                assert_eq!(carried.main, MainCarry::ExtendedPrefix, "{case}");
                assert!(backups(&dst).is_empty(), "{case}");
            }
            _ => {
                let MainCarry::DivergedPreserved { backup } = &carried.main else {
                    panic!(
                        "{case}: expected the fork preserved, got {:?}",
                        carried.main
                    );
                };
                assert_eq!(std::fs::read_to_string(backup).unwrap(), dst_body, "{case}");
            }
        }
    }
}

/// The ranking never consults the mtime. A stale copy with a FRESH mtime
/// (what a carry gives the copy it writes) loses to the newest copy with an
/// old one, both when the stale copy is a strict prefix and when it diverged
/// with an older newest turn. In the incident, the tailer's mtime rule bound
/// exactly such a stale copy.
#[test]
fn the_ranking_never_consults_mtime() {
    for (case, stale, newest) in [
        ("strict prefix", base(), extended()),
        ("older newest turn", fork_old(), fork_new()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst, third) = roots(tmp.path());
        let stale_main = write_main(&src, &stale);
        let newest_main = write_main(&third, &newest);
        set_mtime(&newest_main, two_days_ago());
        set_mtime(&stale_main, SystemTime::now());
        let all = [src.clone(), dst.clone(), third.clone()];

        assert_eq!(rank(&src, &dst, &all, WD, SID), other(&third), "{case}");
        let carried = carry(&all, &src, &dst);
        assert_eq!(carried.carried_from, third, "{case}");
        assert_eq!(read_main(&dst), newest, "{case}");
    }
}

/// When the ranking cannot be established, the carry reads the migration's
/// source, as it did before the ranking existed. The cases: diverged copies
/// with no turn in reach, diverged copies whose newest turns tie, and one
/// diverged copy with a turn against one without (the untimed one is the
/// source, and the timed third root still does not win by default). The
/// third root's copy is the larger one and has the fresher mtime, and it
/// never wins by default.
#[test]
fn an_unrankable_set_of_copies_falls_back_to_the_source() {
    let tie = "2026-11-16T09:00:00.000Z";
    for (case, src_body, third_body) in [
        (
            "no turns",
            untimed("source"),
            untimed("third root, the longer"),
        ),
        (
            "tied newest turns",
            base() + &rec(tie, "src"),
            base() + &rec(tie, "third, the longer"),
        ),
        ("one timed, one untimed", untimed("source"), extended()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst, third) = roots(tmp.path());
        let src_main = write_main(&src, &src_body);
        set_mtime(&src_main, two_days_ago());
        write_main(&third, &third_body);
        let all = [src.clone(), dst.clone(), third.clone()];

        assert!(
            matches!(rank(&src, &dst, &all, WD, SID), Newest::Unknown(_)),
            "{case}: {:?}",
            rank(&src, &dst, &all, WD, SID)
        );
        let carried = carry(&all, &src, &dst);
        assert_eq!(carried.carried_from, src, "{case}");
        assert_eq!(read_main(&dst), src_body, "{case}");
        assert_eq!(carried.main, MainCarry::Copied, "{case}");
    }
}

/// A copy that cannot be read is UNKNOWN, never a loser and never a winner:
/// the carry falls back to the source.
#[cfg(unix)]
#[test]
fn an_unreadable_copy_makes_the_ranking_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    write_main(&src, &base());
    let third_main = write_main(&third, &extended());
    std::fs::set_permissions(&third_main, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&third_main).is_ok() {
        eprintln!(
            "SKIPPED an_unreadable_copy_makes_the_ranking_unknown: file permissions do not stop this process reading (running as root?)"
        );
        return;
    }
    let all = [src.clone(), dst.clone(), third.clone()];
    assert!(matches!(
        rank(&src, &dst, &all, WD, SID),
        Newest::Unknown(_)
    ));
    let carried = carry(&all, &src, &dst);
    std::fs::set_permissions(&third_main, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(carried.carried_from, src);
    assert_eq!(read_main(&dst), base());
}

/// No root holds a copy: UNKNOWN, and the production door's carry from the
/// source then fails exactly as before ("source transcript missing").
#[test]
fn no_copy_anywhere_is_unknown_and_the_carry_fails_as_before() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, _) = roots(tmp.path());
    assert!(matches!(rank(&src, &dst, &[], WD, SID), Newest::Unknown(_)));
    let err = copy_transcript(src.to_str().unwrap(), dst.to_str().unwrap(), WD, SID).unwrap_err();
    assert!(err.contains("source transcript missing"), "{err}");
}

/// A root reached again through a symlink is one copy, kept under the role it
/// was first listed with (the source's), so it is never compared with itself.
/// The roster's other copies follow in roster order.
#[cfg(unix)]
#[test]
fn a_root_reached_again_through_a_symlink_is_one_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    write_main(&src, &base());
    write_main(&third, &extended());
    let link = tmp.path().join(".claude-link");
    std::os::unix::fs::symlink(&src, &link).unwrap();
    assert_ne!(main_path(&link), main_path(&src), "a different spelling");

    let (copies, failures) = copies_of(&src, &dst, &[link, third.clone()], WD, SID);
    assert!(failures.is_empty(), "{failures:?}");
    let seen: Vec<(&Path, Role)> = copies.iter().map(|c| (c.root.as_path(), c.role)).collect();
    assert_eq!(
        seen,
        vec![
            (src.as_path(), Role::Source),
            (third.as_path(), Role::Other)
        ]
    );
    assert_eq!(
        rank(&src, &dst, &[src.clone(), third.clone()], WD, SID),
        other(&third)
    );
}

/// The whole composition through account-root DISCOVERY: a temp home holding
/// `.claude` (the migration's stale source), `.claude-a` (the destination,
/// with an older fork of its own) and `.claude-b` (where the session really
/// ran). The carry enters through [`copy_transcript_discovering`], the
/// function [`copy_transcript`] wraps, so it ranks the roots discovered the
/// way production discovers the real ones. Ranking only the source and the
/// destination would keep the destination (the source is a prefix of it), so
/// the live root is carried only if the discovered roots reach the ranking.
/// Nothing here reads the operator's home or settings.
#[test]
fn a_carry_over_discovered_roots_carries_the_newest_copy() {
    let home = tempfile::tempdir().unwrap();
    let (src, dst, live) = (
        home.path().join(".claude"),
        home.path().join(".claude-a"),
        home.path().join(".claude-b"),
    );
    write_main(&src, &base());
    write_main(&dst, &fork_old());
    write_main(&live, &fork_new());
    let ambient = AmbientDiscovery {
        env_config_dir: None,
        home_dir: Some(home.path().to_path_buf()),
        claude_root: Some(home.path().to_path_buf()),
    };

    assert_eq!(
        crate::terminal::transcript::claude_config_dirs_from(&ambient, &[]),
        vec![dst.clone(), live.clone(), src.clone()]
    );
    let carried = copy_transcript_discovering(
        &ambient,
        &[],
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        WD,
        SID,
    )
    .unwrap();

    assert_eq!(carried.carried_from, live);
    assert_eq!(read_main(&dst), fork_new());
    let MainCarry::DivergedPreserved { backup } = &carried.main else {
        panic!("expected the older fork preserved, got {:?}", carried.main);
    };
    assert_eq!(std::fs::read_to_string(backup).unwrap(), fork_old());
    assert_eq!(read_main(&src), base(), "the source is never touched");
}

/// A1: one unrankable copy in ANY root (here a fourth root's diverged fork
/// with no turn in reach) fails the full ranking, but it does not throw away
/// what the byte prefix order proves among the copies that can be read. A
/// third root PROVEN to strictly extend the source is newer than the source
/// whatever the fourth holds, so it is carried, not the stale source. In the
/// second case the destination extends the source too, and the third root
/// extends the destination: the greatest extension is carried (onto the
/// destination, extending it), not the destination merely kept.
#[test]
fn an_unrankable_fourth_root_does_not_stop_a_proven_extension_being_carried() {
    let further = extended() + &rec("2026-11-16T09:30:00.000Z", "further still");
    for (case, dst_body, third_body, main) in [
        ("no destination", None, extended(), MainCarry::Copied),
        (
            "the destination extends the source too",
            Some(extended()),
            further.clone(),
            MainCarry::ExtendedPrefix,
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst, third) = roots(tmp.path());
        let fourth = tmp.path().join(".claude-zed");
        write_main(&src, &base());
        if let Some(body) = &dst_body {
            write_main(&dst, body);
        }
        write_main(&third, &third_body);
        write_main(&fourth, &untimed("fourth"));
        let all = [src.clone(), dst.clone(), third.clone(), fourth.clone()];

        let verdict = rank(&src, &dst, &all, WD, SID);
        assert!(
            matches!(&verdict, Newest::Other { root, unranked: Some(_) } if *root == third),
            "{case}: {verdict:?}"
        );
        let carried = carry(&all, &src, &dst);
        assert_eq!(carried.carried_from, third, "{case}");
        assert_eq!(read_main(&dst), third_body, "{case}");
        assert_eq!(carried.main, main, "{case}");
        assert_eq!(
            read_main(&src),
            base(),
            "{case}: the source is never touched"
        );
    }
}

/// A2: byte-identical copies form one class, and a copy identical to an
/// earlier class is not compared with the classes after it. Three classes
/// from five copies: the source and a copy of it, an extension and a copy of
/// it, and a fork that extends the source but diverged from the extension.
/// Each pair of classes relates both ways.
#[test]
fn identical_copies_form_one_class_compared_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (src, dst, third) = roots(tmp.path());
    let (twin, fork) = (
        tmp.path().join(".claude-twin"),
        tmp.path().join(".claude-fork"),
    );
    write_main(&src, &base());
    write_main(&dst, &extended());
    write_main(&third, &extended());
    write_main(&twin, &base());
    write_main(&fork, &fork_old());
    let (copies, failures) = copies_of(
        &src,
        &dst,
        &[third.clone(), twin.clone(), fork.clone()],
        WD,
        SID,
    );
    assert!(failures.is_empty(), "{failures:?}");

    let classes = classify(&copies);
    assert_eq!(classes.class_of, vec![0, 1, 1, 0, 2]);
    assert_eq!(classes.count(), 3);
    assert!(classes.failures.is_empty(), "{:?}", classes.failures);
    assert!(classes.older(0, 1) && !classes.older(1, 0));
    assert_eq!(classes.rel[1][0], Some(Order::Newer));
    assert!(classes.older(0, 2));
    assert_eq!(classes.rel[2][1], Some(Order::Diverged));
}
