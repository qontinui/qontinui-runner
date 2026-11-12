//! Tests for the aftermath record ([`super`]; plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
//! Phase 2). Each is shown red by a mutation of the production code it names.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::session::session_ledger::{LedgerEntry, SessionLedger, LEDGER_VERSION, REASON_POLL};
use crate::session::session_lifecycle_store::{
    PollTerminalMatch, SessionLifecycleStore, TerminalSessionRecord,
};

/// The generation file the boots under test observe.
const GEN: &str = "session-ledger.1000.json";
/// The boot under test.
const BOOT: i64 = 2_000;

use PollTerminalMatch::{Direct, Fallback, Unmatched};

fn roster_entry(id: &str, finished: bool) -> LedgerEntry {
    LedgerEntry {
        claude_session_id: id.to_string(),
        terminal_id: format!("term-{id}"),
        page_id: "default".to_string(),
        zone_index: 0,
        title: None,
        session_name: None,
        name_source: None,
        account_label: None,
        config_dir: None,
        resume_config_dir: None,
        resume_command: None,
        provider: Some("claude".to_string()),
        working_dir: None,
        last_seen_at: None,
        finished,
        restorable: Some(true),
        worktree_path: None,
        plan_slug: None,
        work_unit_id: None,
        wip_state: None,
        wip_ref: None,
        custody_session_mismatch: false,
    }
}

/// The prior roster: every id unfinished.
fn roster(ids: &[&str]) -> SessionLedger {
    SessionLedger {
        ledger_version: LEDGER_VERSION,
        captured_at_ms: 1_000,
        captured_at: "2026-11-11T00:00:01Z".to_string(),
        reason: REASON_POLL.to_string(),
        boot_at_ms: Some(10),
        shutdown_at: None,
        clean_shutdown: Some(true),
        sessions: ids.iter().map(|id| roster_entry(id, false)).collect(),
    }
}

fn open_at(dir: &Path, boot_ms: i64, prior: &SessionLedger) -> LedgerAftermath {
    let retained: HashSet<String> = [GEN.to_string()].into_iter().collect();
    LedgerAftermath::open(dir, boot_ms, Some(GEN.to_string()), Some(prior), &retained)
}

/// An open, confirmed registry row on its own pane.
fn row(id: &str) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: id.to_string(),
        config_dir: Some("C:/claude/.claude-ben".to_string()),
        working_dir: Some("C:/repo".to_string()),
        page_id: "default".to_string(),
        zone_index: 0,
        title: Some("claude".to_string()),
        terminal_id: format!("term-{id}"),
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: "claude".to_string(),
        origin: Some("authoritative".to_string()),
        restore_pending_at: None,
        awaiting_account_since: None,
        confirmed_at: Some(1),
        handle: None,
        account_label: Some("ben".to_string()),
        account_wrapper: None,
        session_name: Some(format!("name-{id}")),
        name_source: None,
        tenant_id: None,
        task_run_id: None,
        bypass_permissions: None,
        restored_from_boot_at: None,
        restore_tier: None,
        finished_at: None,
        wind_down_outcome: None,
        wind_down_at: None,
        wind_down_arm: None,
        finish_reason: None,
        finish_synced: false,
        spawn_device_default: None,
        adopted_from: None,
        last_session_start_at: None,
        last_alive_at: None,
        gate_id: None,
        gate_consuming_device_id: None,
        gate_bound_boot_ms: None,
    }
}

fn closed_row(id: &str, at: i64, reason: &str) -> TerminalSessionRecord {
    TerminalSessionRecord {
        state: "closed".to_string(),
        closed_at: Some(at),
        close_reason: Some(reason.to_string()),
        ..row(id)
    }
}

fn finished_row(id: &str, at: i64) -> TerminalSessionRecord {
    TerminalSessionRecord {
        finished_at: Some(at),
        ..row(id)
    }
}

/// This boot's record as the classification reads it.
fn this_boot(a: &LedgerAftermath) -> AftermathRecord {
    a.inputs(BOOT).this_boot.expect("an opened record")
}

fn on_disk(dir: &Path, boot_ms: i64) -> Option<AftermathRecord> {
    match read_record_file(&dir.join(aftermath_file_name(boot_ms))) {
        RecordFile::Ok(r) => Some(r),
        RecordFile::Unreadable | RecordFile::Io => None,
    }
}

fn held(ids: &[&str]) -> ProcessRead {
    ProcessRead::Held(ids.iter().map(|s| s.to_string()).collect())
}

/// A healthy process table holding `pids` (each a `claude`).
fn live_snapshot(pids: &[u32]) -> ProcessSnapshot {
    let mut snap = ProcessSnapshot {
        stale: false,
        observed_at_ms: chrono::Utc::now().timestamp_millis(),
        ..ProcessSnapshot::default()
    };
    snap.names.insert(1, "init".to_string());
    for pid in pids {
        snap.names.insert(*pid, "claude".to_string());
    }
    snap.parent_map.insert(1, pids.to_vec());
    snap
}

/// A Claude Code config dir whose live-session registry says `pid` runs
/// `session_id`.
fn config_dir_holding(root: &Path, pid: u32, session_id: &str) -> PathBuf {
    let config = root.join(".claude-ben");
    std::fs::create_dir_all(config.join("sessions")).unwrap();
    std::fs::write(
        config.join("sessions").join(format!("{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session_id}","cwd":"C:/repo","kind":"interactive","name":"n","status":"busy"}}"#
        ),
    )
    .unwrap();
    config
}

/// Mutation: `SessionAftermath::came_back` overwrites an earlier observation
/// (its `is_some` early return removed) — the second tick moves the instant.
#[test]
fn a_keep_alive_on_a_direct_match_is_recorded_once_with_its_first_instant() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["s1", "s2"]));

    assert!(a.observe_tick(3_000, &[("s1".to_string(), Direct)], held(&[])));
    assert!(
        !a.observe_tick(3_045, &[("s1".to_string(), Direct)], held(&[])),
        "seeing it alive again records nothing new"
    );

    let e = this_boot(&a).sessions["s1"].clone();
    assert_eq!(e.came_back_at, Some(3_000), "its FIRST instant");
    assert_eq!(e.came_back_by, Some(CameBackBy::Bound));
    assert_eq!(
        on_disk(dir.path(), BOOT).unwrap().sessions["s1"].came_back_at,
        Some(3_000),
        "and that is what the file says"
    );
    assert!(
        !this_boot(&a).sessions.contains_key("s2"),
        "a session nothing saw has no entry"
    );
}

/// Mutation: `observe_tick` records any `KeepAlive`, not only a `Direct` match
/// (its `!= PollTerminalMatch::Direct` skip removed).
#[test]
fn a_keep_alive_through_the_fallback_match_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["s1"]));

    assert!(!a.observe_tick(
        3_000,
        &[("s1".to_string(), Fallback), ("s1".to_string(), Unmatched)],
        held(&[]),
    ));

    assert!(
        this_boot(&a).sessions.is_empty(),
        "a (page, title, cwd) match is another tab, not this row's terminal"
    );
    assert!(
        on_disk(dir.path(), BOOT).is_none(),
        "nothing observed, nothing written"
    );
}

/// Mutation: `observe_registry` records every row, not only prior-roster ones
/// (its roster filter removed); and, separately, the mirror keeps a close the
/// registry no longer shows (the `if row.open { None }` arm made `row.close`).
#[test]
fn a_roster_ids_close_and_finish_are_recorded_with_their_reasons_and_a_non_roster_id_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    for id in ["closed-one", "finished-one", "stranger"] {
        store.record_open(row(id));
    }
    store.record_close("closed-one", "explicit");
    store.record_close("stranger", "explicit");
    store
        .set_finished("finished-one", true, None)
        .expect("a known session");
    let a = open_at(dir.path(), BOOT, &roster(&["closed-one", "finished-one"]));

    assert!(a.observe_registry(&store.all_records()));

    let rec = this_boot(&a);
    let closed = &rec.sessions["closed-one"];
    assert_eq!(closed.close_reason.as_deref(), Some("explicit"));
    assert_eq!(closed.closed_at, store.get("closed-one").unwrap().closed_at);
    assert!(closed.closed_at.is_some());
    assert_eq!(closed.finished_at, None);
    let finished = &rec.sessions["finished-one"];
    assert_eq!(
        finished.finished_at,
        store.get("finished-one").unwrap().finished_at
    );
    assert!(finished.finished_at.is_some());
    assert_eq!(finished.close_reason, None);
    assert!(
        !rec.sessions.contains_key("stranger"),
        "a session not on the prior roster is not recorded"
    );

    // The registry is the authority while it holds the row: a re-opened row
    // clears its recorded close, an unfinished one its recorded finish.
    store.record_open(row("closed-one"));
    store
        .set_finished("finished-one", false, None)
        .expect("a known session");
    assert!(a.observe_registry(&store.all_records()));
    let rec = this_boot(&a);
    assert_eq!(rec.sessions["closed-one"].close(), None);
    assert_eq!(rec.sessions["finished-one"].finished_at, None);
}

/// Mutations: `observe_tick` records a held id whatever the read (the
/// `ProcessRead::Failed` arm made to record the previous tick's set), and
/// `ProcessRead::of_tick` ignores the snapshot verdict (its `if snapshot_ok`
/// guard removed).
#[test]
fn a_held_id_is_recorded_only_from_a_successful_read_and_a_failed_read_records_nothing_and_reads_unknown(
) {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["held", "other"]));

    assert!(!a.observe_tick(3_000, &[], ProcessRead::Failed));
    assert!(
        this_boot(&a).sessions.is_empty(),
        "a failed read records nothing"
    );
    assert_eq!(a.inputs(3_000).process, ProcessEvidence::Unknown);

    assert!(a.observe_tick(3_045, &[], held(&["HELD", "not-on-roster"])));
    let rec = this_boot(&a);
    assert_eq!(rec.sessions["held"].came_back_at, Some(3_045));
    assert_eq!(rec.sessions["held"].came_back_by, Some(CameBackBy::Process));
    assert!(!rec.sessions.contains_key("not-on-roster"));
    assert_eq!(
        a.inputs(3_045).process,
        ProcessEvidence::Held(
            ["held".to_string(), "not-on-roster".to_string()]
                .into_iter()
                .collect()
        ),
        "the newest read's whole set, lowercased"
    );

    // A later failed read takes nothing back, and the tick is UNKNOWN again.
    assert!(!a.observe_tick(3_090, &[], ProcessRead::Failed));
    assert_eq!(this_boot(&a).sessions["held"].came_back_at, Some(3_045));
    assert_eq!(a.inputs(3_090).process, ProcessEvidence::Unknown);

    // The tick's read: a failed snapshot fails it whatever the registry said.
    let config = config_dir_holding(dir.path(), 4242, "held");
    let rows = read_live_claude_sessions(&live_snapshot(&[4242]), || vec![config.clone()]);
    assert_eq!(ProcessRead::of_tick(true, &rows), held(&["held"]));
    assert_eq!(ProcessRead::of_tick(false, &rows), ProcessRead::Failed);
    let indeterminate =
        read_live_claude_sessions(&ProcessSnapshot::default(), || vec![config.clone()]);
    assert!(
        indeterminate.is_err(),
        "an indeterminate snapshot fails the read closed"
    );
    assert_eq!(
        ProcessRead::of_tick(true, &indeterminate),
        ProcessRead::Failed
    );
}

/// A registry read that is only PARTIAL — here a live `claude`'s
/// `<pid>.json` that does not parse, so the session that pid hosts is unseen —
/// still proves the sessions it found are back, but cannot say who else is
/// held, so the tick's process evidence is UNKNOWN. Mutations:
/// `ProcessRead::of_tick` ignoring `LiveSessions::is_complete` (a partial read
/// served as `Held`), and `observe_tick` serving a `Partial` read's ids as the
/// tick's held set (`TickRead::Held` instead of `TickRead::Failed`).
#[test]
fn a_partial_registry_read_records_what_it_found_and_leaves_the_tick_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["held", "unseen"]));
    let config = config_dir_holding(dir.path(), 4242, "held");
    std::fs::write(config.join("sessions").join("5151.json"), "{ torn").unwrap();

    let read = read_live_claude_sessions(&live_snapshot(&[4242, 5151]), || vec![config.clone()]);
    let complete = read.as_ref().map(LiveSessions::is_complete);
    assert_eq!(
        complete,
        Ok(false),
        "the torn record leaves the read partial"
    );
    let process = ProcessRead::of_tick(true, &read);
    assert_eq!(process, ProcessRead::Partial(vec!["held".to_string()]));

    assert!(a.observe_tick(3_000, &[], process));
    let rec = this_boot(&a);
    assert_eq!(rec.sessions["held"].came_back_at, Some(3_000));
    assert_eq!(rec.sessions["held"].came_back_by, Some(CameBackBy::Process));
    assert!(!rec.sessions.contains_key("unseen"));
    assert_eq!(
        a.inputs(3_000).process,
        ProcessEvidence::Unknown,
        "a session the partial read could not see may be held"
    );
}

/// Mutation: `observe_idle_tick` always takes a snapshot (its
/// `wants_process_read` early return removed), and `wants_process_read` stays
/// armed by a settled session (`SessionAftermath::unsettled` made `true`).
#[tokio::test]
async fn an_idle_tick_reads_while_a_roster_session_is_unsettled_and_takes_no_snapshot_once_none_is()
{
    let dir = tempfile::tempdir().unwrap();
    let config = config_dir_holding(dir.path(), 4242, "held");
    let a = open_at(
        dir.path(),
        BOOT,
        &roster(&["held", "finishes-later", "closes-later"]),
    );
    let snapshots = AtomicUsize::new(0);
    let take = || async {
        snapshots.fetch_add(1, Ordering::SeqCst);
        live_snapshot(&[4242])
    };

    // No open row anywhere: the idle path still reads, and the held session is
    // recorded as back.
    assert!(a.wants_process_read());
    assert_eq!(
        a.observe_idle_tick(take, || vec![config.clone()]).await,
        IdleTick::Read
    );
    assert_eq!(snapshots.load(Ordering::SeqCst), 1);
    assert_eq!(
        this_boot(&a).sessions["held"].came_back_by,
        Some(CameBackBy::Process)
    );

    // The other two settle without coming back: one finished, one closed.
    a.observe_registry(&[
        finished_row("finishes-later", 5_000),
        closed_row("closes-later", 5_000, "no-terminal"),
    ]);
    assert!(!a.wants_process_read());
    let take = || async {
        snapshots.fetch_add(1, Ordering::SeqCst);
        live_snapshot(&[4242])
    };
    assert_eq!(
        a.observe_idle_tick(take, || vec![config.clone()]).await,
        IdleTick::SkippedByDesign
    );
    assert_eq!(
        snapshots.load(Ordering::SeqCst),
        1,
        "once every roster session is observed, finished or closed, the idle tick takes no snapshot"
    );
    assert_eq!(
        a.inputs(chrono::Utc::now().timestamp_millis()).process,
        ProcessEvidence::NotNeeded,
        "a tick skipped by design is not UNKNOWN"
    );
}

/// An idle tick whose snapshot fails records nothing and reads UNKNOWN.
#[tokio::test]
async fn an_idle_tick_whose_snapshot_fails_records_nothing_and_reads_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_dir_holding(dir.path(), 4242, "held");
    let a = open_at(dir.path(), BOOT, &roster(&["held"]));

    let failed = || async { ProcessSnapshot::default() };
    assert_eq!(
        a.observe_idle_tick(failed, || vec![config.clone()]).await,
        IdleTick::Failed
    );
    assert!(this_boot(&a).sessions.is_empty());
    assert_eq!(
        a.inputs(chrono::Utc::now().timestamp_millis()).process,
        ProcessEvidence::Unknown
    );
    assert!(
        a.wants_process_read(),
        "still unobserved: the next idle tick reads again"
    );
}

/// Mutation: `observe_registry` drops the entries of roster ids the registry
/// no longer holds — the record would forget a session once its row is pruned.
#[test]
fn the_record_outlives_the_rows_prune() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    store.record_open(row("came-back"));
    let a = open_at(dir.path(), BOOT, &roster(&["came-back"]));
    a.observe_registry(&store.all_records());
    a.observe_tick(3_000, &[("came-back".to_string(), Direct)], held(&[]));
    store.record_close("came-back", "explicit");
    a.observe_registry(&store.all_records());

    store.prune(
        chrono::Utc::now().timestamp_millis() + 2 * 86_400_000,
        &HashSet::new(),
    );
    assert!(
        store.get("came-back").is_none(),
        "fixture: the prune dropped the closed row"
    );
    assert!(
        !a.observe_registry(&store.all_records()),
        "the row's absence changes nothing recorded"
    );

    for rec in [this_boot(&a), on_disk(dir.path(), BOOT).unwrap()] {
        let e = &rec.sessions["came-back"];
        assert_eq!(e.came_back_at, Some(3_000), "still back");
        assert_eq!(e.close_reason.as_deref(), Some("explicit"), "still closed");
    }
}

/// Mutation: the record's file named by a fixed stamp instead of its boot
/// (`aftermath_file_name(boot_ms)` in `open` made `aftermath_file_name(0)`),
/// so the second boot overwrites the first's.
#[test]
fn two_boots_naming_one_generation_file_keep_separate_records() {
    let dir = tempfile::tempdir().unwrap();
    let prior = roster(&["s1", "s2"]);
    let first = open_at(dir.path(), BOOT, &prior);
    first.observe_tick(3_000, &[("s1".to_string(), Direct)], held(&[]));
    let first_bytes = std::fs::read(dir.path().join(aftermath_file_name(BOOT))).unwrap();

    // rotate_in declined to duplicate the unchanged roster: the next boot
    // observes the same generation file.
    let second = open_at(dir.path(), BOOT + 1_000, &prior);
    second.observe_tick(4_000, &[("s2".to_string(), Direct)], held(&[]));

    assert_eq!(
        std::fs::read(dir.path().join(aftermath_file_name(BOOT))).unwrap(),
        first_bytes,
        "the first boot's record is untouched"
    );
    let first_rec = on_disk(dir.path(), BOOT).unwrap();
    let second_rec = on_disk(dir.path(), BOOT + 1_000).unwrap();
    assert_eq!(first_rec.generation_file.as_deref(), Some(GEN));
    assert_eq!(second_rec.generation_file.as_deref(), Some(GEN));
    assert!(first_rec.sessions.contains_key("s1") && !first_rec.sessions.contains_key("s2"));
    assert!(second_rec.sessions.contains_key("s2") && !second_rec.sessions.contains_key("s1"));
    let earlier = second.inputs(BOOT).earlier;
    assert_eq!(
        earlier.iter().map(|r| r.boot_ms).collect::<Vec<_>>(),
        vec![BOOT],
        "the second boot reads the first's record as an earlier one"
    );
}

/// Mutation: `persist_if_dirty` writes on every call (both its `dirty` and
/// its unchanged-bytes early returns removed).
#[test]
fn the_file_is_rewritten_only_when_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["s1", "s2"]));
    let rows = [row("s1"), closed_row("s2", 2_500, "pty-exit")];

    assert!(
        a.observe_registry(&rows),
        "the first observation is written"
    );
    let path = dir.path().join(aftermath_file_name(BOOT));
    let bytes = std::fs::read(&path).unwrap();

    assert!(
        !a.observe_registry(&rows),
        "the same registry is not rewritten"
    );
    assert!(
        !a.observe_tick(3_000, &[], held(&["someone-else"])),
        "a tick that observes nothing new is not written"
    );
    assert!(!a.observe_tick(3_045, &[], ProcessRead::SkippedByDesign));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    assert!(a.observe_tick(3_090, &[("s1".to_string(), Direct)], held(&[])));
    assert_ne!(std::fs::read(&path).unwrap(), bytes, "a change is written");
}

/// Zero, one and many prior sessions. Mutation: `open` keeps only the first
/// roster entry (`.take(1)` on the roster) — the many-session counts fail.
#[test]
fn zero_one_and_many_sessions() {
    // Zero: nothing to record, nothing to read, nothing written.
    let dir = tempfile::tempdir().unwrap();
    let none = open_at(dir.path(), BOOT, &roster(&[]));
    assert!(!none.observe_registry(&[row("anyone")]));
    assert!(!none.observe_tick(3_000, &[("anyone".to_string(), Direct)], held(&["anyone"])));
    assert!(!none.wants_process_read());
    assert!(on_disk(dir.path(), BOOT).is_none());

    // One.
    let dir = tempfile::tempdir().unwrap();
    let one = open_at(dir.path(), BOOT, &roster(&["only"]));
    assert!(one.wants_process_read());
    assert!(one.observe_tick(3_000, &[("only".to_string(), Direct)], held(&[])));
    assert!(!one.wants_process_read());
    assert_eq!(on_disk(dir.path(), BOOT).unwrap().sessions.len(), 1);

    // Many: 60 sessions, a third kept alive, a third held by a live claude,
    // a third closed without coming back.
    let dir = tempfile::tempdir().unwrap();
    let ids: Vec<String> = (0..60).map(|i| format!("s{i:02}")).collect();
    let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let many = open_at(dir.path(), BOOT, &roster(&id_refs));
    let kept: Vec<(String, PollTerminalMatch)> =
        ids[..20].iter().map(|id| (id.clone(), Direct)).collect();
    let holding: Vec<&str> = id_refs[20..40].to_vec();
    many.observe_tick(3_000, &kept, held(&holding));
    let closes: Vec<TerminalSessionRecord> = ids[40..]
        .iter()
        .map(|id| closed_row(id, 3_500, "no-terminal"))
        .collect();
    many.observe_registry(&closes);
    let rec = on_disk(dir.path(), BOOT).unwrap();
    assert_eq!(rec.sessions.len(), 60);
    let by = |by: CameBackBy| {
        rec.sessions
            .values()
            .filter(|e| e.came_back_by == Some(by))
            .count()
    };
    assert_eq!(by(CameBackBy::Bound), 20);
    assert_eq!(by(CameBackBy::Process), 20);
    assert_eq!(
        rec.sessions
            .values()
            .filter(|e| e.came_back_at.is_none() && e.close_reason.as_deref() == Some("no-terminal"))
            .count(),
        20
    );
    assert!(!many.wants_process_read());
}

/// Mutation: `observe_tick` keys a kept-alive entry by the raw id rather than
/// the lowercased one — the same session, seen twice, becomes two entries.
#[test]
fn the_same_observation_made_twice_is_one_entry() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["s1"]));

    // One tick sees it on its own terminal AND held by a live claude, under
    // two spellings of its id; the next tick sees the same again.
    for at in [3_000, 3_045] {
        a.observe_tick(at, &[("S1".to_string(), Direct)], held(&["s1", "S1"]));
    }
    let rec = this_boot(&a);
    assert_eq!(rec.sessions.len(), 1);
    assert_eq!(rec.sessions["s1"].came_back_at, Some(3_000));
    assert_eq!(rec.sessions["s1"].came_back_by, Some(CameBackBy::Bound));
}

/// Mutations: `retain_earlier` keeps a record whatever generation it names
/// (its retained-generation guard removed), keeps every record of a
/// generation (the per-generation cap removed), and touches this boot's own
/// file (its `stamp == boot_ms` skip removed).
#[test]
fn a_record_is_retained_with_its_generation_and_pruned_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let write = |boot_ms: i64, generation: &str| {
        let rec = AftermathRecord {
            aftermath_version: AFTERMATH_VERSION,
            boot_ms,
            generation_file: Some(generation.to_string()),
            sessions: BTreeMap::new(),
        };
        std::fs::write(
            dir.path().join(aftermath_file_name(boot_ms)),
            serde_json::to_vec(&rec).unwrap(),
        )
        .unwrap();
    };
    write(10, GEN);
    write(11, "session-ledger.999.json");
    for boot in 20..=26 {
        write(boot, GEN);
    }
    std::fs::write(dir.path().join(aftermath_file_name(12)), b"not a record").unwrap();
    // A file already named for the opening boot is never pruned at open.
    std::fs::write(dir.path().join(aftermath_file_name(100)), b"own").unwrap();
    // Not records at all: never listed, never pruned.
    std::fs::write(dir.path().join(GEN), b"{}").unwrap();
    std::fs::write(dir.path().join("session-ledger-aftermath.json"), b"x").unwrap();

    let a = open_at(dir.path(), 100, &roster(&["s1"]));

    let earlier: Vec<i64> = a.inputs(100).earlier.iter().map(|r| r.boot_ms).collect();
    assert_eq!(
        earlier,
        vec![26, 25, 24, 23, 22],
        "the newest RECORDS_KEPT_PER_GENERATION records of the retained generation"
    );
    let mut left: Vec<i64> = record_files_in(dir.path())
        .into_iter()
        .map(|(b, _)| b)
        .collect();
    left.sort_unstable();
    assert_eq!(left, vec![22, 23, 24, 25, 26, 100]);
    assert_eq!(
        std::fs::read(dir.path().join(aftermath_file_name(100))).unwrap(),
        b"own"
    );
    assert!(dir.path().join(GEN).exists());
    assert!(dir.path().join("session-ledger-aftermath.json").exists());
}

/// Fail-soft. Mutation: `persist_if_dirty` panics on a write error
/// (`write_record(..).expect(..)`).
#[test]
fn a_write_failure_is_logged_and_never_fatal() {
    let dir = tempfile::tempdir().unwrap();
    // The record's directory is a FILE: every write fails.
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, b"a file, not a directory").unwrap();
    let a = open_at(&blocked, BOOT, &roster(&["s1"]));

    assert!(!a.observe_tick(3_000, &[("s1".to_string(), Direct)], held(&[])));
    assert_eq!(
        this_boot(&a).sessions["s1"].came_back_at,
        Some(3_000),
        "kept in memory for the report"
    );
    assert!(
        !a.observe_registry(&[row("s1")]),
        "the retry fails again, quietly"
    );
}

/// Mutations: `process_evidence` never ages a read (its
/// `PROCESS_EVIDENCE_MAX_AGE_MS` guard removed), and ages a by-design skip
/// like a read (`SkippedByDesign` mapped to `Unknown`).
#[test]
fn the_newest_read_counts_for_two_tick_intervals_and_a_by_design_skip_never_ages_into_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["s1"]));
    assert_eq!(
        a.inputs(BOOT).process,
        ProcessEvidence::Unknown,
        "no tick yet"
    );

    a.observe_tick(10_000, &[], held(&["x"]));
    let x: HashSet<String> = ["x".to_string()].into_iter().collect();
    assert_eq!(
        a.inputs(10_000 + PROCESS_EVIDENCE_MAX_AGE_MS).process,
        ProcessEvidence::Held(x)
    );
    assert_eq!(
        a.inputs(10_000 + PROCESS_EVIDENCE_MAX_AGE_MS + 1).process,
        ProcessEvidence::Unknown,
        "older than two tick intervals"
    );
    assert_eq!(
        PROCESS_EVIDENCE_MAX_AGE_MS,
        2 * 45_000,
        "two of the poll's 45 s ticks"
    );

    a.observe_tick(20_000, &[], ProcessRead::SkippedByDesign);
    assert_eq!(
        a.inputs(20_000 + 100 * PROCESS_EVIDENCE_MAX_AGE_MS).process,
        ProcessEvidence::NotNeeded
    );
}

/// Mutation: `observe_registry` takes any restore stamp, not only this
/// boot's (its `>= self.boot_ms` filter removed).
#[test]
fn a_restore_stamp_from_this_boot_is_came_back_by_stamp_and_an_earlier_boots_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["restored", "old-stamp"]));
    let stamped = |id: &str, at: i64| TerminalSessionRecord {
        restored_from_boot_at: Some(at),
        ..row(id)
    };

    a.observe_registry(&[
        stamped("restored", BOOT + 5),
        stamped("old-stamp", BOOT - 1),
    ]);

    let rec = this_boot(&a);
    assert_eq!(rec.sessions["restored"].came_back_at, Some(BOOT + 5));
    assert_eq!(
        rec.sessions["restored"].came_back_by,
        Some(CameBackBy::Stamp)
    );
    assert_eq!(
        rec.sessions["old-stamp"].came_back_at, None,
        "a stamp from an earlier boot says nothing about this one"
    );
}

/// The file's wire shape, pinned (the record is read back by later boots, and
/// by an operator).
#[test]
fn the_record_file_names_its_boot_and_generation_and_uses_camel_case_keys() {
    let dir = tempfile::tempdir().unwrap();
    let a = open_at(dir.path(), BOOT, &roster(&["s1"]));
    a.observe_tick(3_000, &[("s1".to_string(), Direct)], held(&[]));
    a.observe_registry(&[TerminalSessionRecord {
        finished_at: Some(4_000),
        ..closed_row("s1", 3_500, "explicit")
    }]);

    let raw: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("session-ledger-aftermath.2000.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        raw,
        serde_json::json!({
            "aftermathVersion": 1,
            "bootMs": 2000,
            "generationFile": "session-ledger.1000.json",
            "sessions": {
                "s1": {
                    "cameBackAt": 3000,
                    "cameBackBy": "bound",
                    "closedAt": 3500,
                    "closeReason": "explicit",
                    "finishedAt": 4000
                }
            }
        })
    );
    assert_eq!(
        parse_aftermath_file_name("session-ledger-aftermath.2000.json"),
        Some(2_000)
    );
    assert_eq!(
        parse_aftermath_file_name("session-ledger-aftermath.json"),
        None
    );
    assert_eq!(parse_aftermath_file_name("session-ledger.2000.json"), None);
    assert_eq!(
        parse_aftermath_file_name("session-ledger-aftermath.2000.json.tmp.1"),
        None
    );
}
