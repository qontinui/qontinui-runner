#![cfg(test)]

use super::*;
use tempfile::tempdir;

/// Terminal liveness for [`SessionLifecycleStore::mark_awaiting_account`]
/// in tests where no PTY of the process is live.
const NONE_LIVE: fn(&str) -> bool = |_| false;

/// Characterization of the WRITE side, which
/// `2026-08-10-temp-runner-session-restore-isolation` Phase 4 did NOT change
/// — [`snapshot_history_path`] was already instance-scoped, so this test
/// passes verbatim against pre-Phase-4 code and does NOT gate that phase.
/// It is kept as a contract pin for the property the readers now depend on:
/// a secondary's history is not under the unscoped hook dir at all, and two
/// spawns on one recycled port do not share a file.
///
/// **The test that actually gates Phase 4 is
/// [`readers_must_not_re_derive_the_snapshot_history_path`].** Phase 4's
/// change was entirely in the two READERS
/// (`commands/terminal.rs:terminal_session_list_history`,
/// `mcp/sessions.rs:list_history`), which used to derive
/// `session-snapshots[-<port>].jsonl` under
/// [`crate::session::claude_hook::session_restore_dir`]. For the primary on
/// 9876 the two coincide, which is why the divergence stayed invisible; for
/// ANY secondary they are a different DIRECTORY *and* a different FILENAME,
/// so the readers opened a file nothing wrote — and on a recycled
/// temp-runner port, a prior temp's history.
///
/// Sets only `QONTINUI_INSTANCE_NAME`, which `resolve_data_subdir` answers
/// on its first branch without reading the port that `scheduler_service`'s
/// tests mutate concurrently.
#[test]
fn snapshot_history_path_is_instance_scoped_and_outside_the_hook_dir() {
    let _amb = crate::test_env::isolated_ambient();
    let _restore = crate::test_env::EnvVarRestore::capture(&["QONTINUI_INSTANCE_NAME"]);

    std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6faa3bf8-0");
    let secondary = snapshot_history_path();
    let hook_dir = crate::session::claude_hook::session_restore_dir();
    assert!(
        !secondary.starts_with(&hook_dir),
        "a secondary's snapshot history must not live in the UNSCOPED hook dir {} (got {})",
        hook_dir.display(),
        secondary.display()
    );
    assert!(
        secondary.ends_with("session-snapshots.jsonl"),
        "the filename stays plain — the instance lives in the directory"
    );

    // Two spawns on one recycled port must not share the file.
    std::env::set_var("QONTINUI_INSTANCE_NAME", "test-19f6fd50c26-2");
    assert_ne!(secondary, snapshot_history_path());
}

/// Finding 3 (code review 2026-08-10): grep-shaped guard for what Phase 4
/// ACTUALLY changed — the two readers. Follows the precedent the plan cites,
/// `qontinui-supervisor` `process/claude_env.rs:every_known_spawn_site_file_still_calls_the_strip`.
///
/// The failure it exists to catch: a future reader (a new MCP route, a
/// diagnostic command) re-derives `session_restore_dir().join("session-snapshots…")`
/// — which is exactly the shape the neighbouring [`crate::session::snapshot_history::tree_reset_path_for_port`]
/// still legitimately uses — and silently re-introduces the divergence for
/// every secondary. Silent because the wrong file simply does not exist, so
/// it reads as "no history" rather than as a bug. Until this test, the only
/// thing preventing that was a comment.
///
/// Three assertions:
/// 1. Both readers resolve through [`snapshot_history_path`].
/// 2. The deleted port-keyed reader helper has not come back.
/// 3. Outside the two modules that legitimately own this pairing (this file,
///    the writer; and `snapshot_history.rs`, which documents it and hosts
///    the port-keyed tree-reset sibling), no source file mentions BOTH
///    `session_restore_dir` and the `session-snapshots` filename — a reader
///    re-deriving the path has to name both.
///
/// The needle for assertion 2 is assembled with `concat!` and appears
/// nowhere in this file as a contiguous literal, deliberately: a
/// grep-shaped guard that writes its own needle in prose flags ITSELF, which
/// is how the first draft of this test failed.
///
/// ## What this guard does NOT cover
///
/// It catches the NAMED shape, not all re-derivation. Two known gaps, stated
/// so nobody reads a green here as broader assurance than it is:
/// - Assertion 1 is satisfied by a *mention* of `snapshot_history_path()`
///   anywhere in the file, including inside a doc comment. A reader that
///   keeps the comment while re-deriving the path elsewhere still passes.
/// - Assertion 3's needle is the co-occurrence of `session_restore_dir` and
///   `session-snapshots`. A path hand-rolled without naming the helper is
///   invisible to it. That evasion shape already exists in-tree:
///   `instance.rs` composes
///   `base.join("session-restore").join("session-snapshots.jsonl")` and this
///   guard does not see it. That instance is benign — it is a pure
///   path-composition test with no runtime reader behind it — but it proves
///   the gap is reachable rather than theoretical.
#[test]
fn readers_must_not_re_derive_the_snapshot_history_path() {
    // Split so this file does not contain the literal it searches for.
    const DELETED_HELPER: &str = concat!("snapshot_path", "_for_port");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    // Anchored to the real tree size (1486 files at the time of writing).
    // A low floor is worse than none: it lets a whole subtree drop out of
    // the walk while the assert still passes — `src/mcp/` alone is 152
    // files and `src/commands/` 112.
    assert!(
        files.len() > 1_000,
        "sanity: expected to walk the whole src tree, found only {} files — \
         a subtree is missing and this guard has scanned less than it thinks",
        files.len()
    );

    // 1. Both readers go through the write-side helper.
    for reader in ["commands/terminal.rs", "mcp/sessions.rs"] {
        let body = std::fs::read_to_string(src.join(reader))
            .unwrap_or_else(|e| panic!("reading {reader}: {e}"));
        assert!(
            body.contains("snapshot_history_path()"),
            "{reader} must resolve the snapshot history through \
             session_lifecycle_store::snapshot_history_path()"
        );
    }

    // 2 + 3. Exempt by RELATIVE PATH, never by basename: a future
    // `src/mcp/snapshot_history.rs` or `src/diagnostics/session_lifecycle_store.rs`
    // would otherwise be silently exempt from assertion 3 — precisely the
    // "new reader in a new file" case this guard exists for, defeated by
    // choice of filename.
    //
    // The store owns the pairing as a MODULE, not as one file: its root
    // `session/session_lifecycle_store.rs` and every submodule under
    // `session/session_lifecycle_store/` (this test module itself, in
    // `tests.rs`, names both needles; later slices move production code such
    // as `snapshot_history_path` there too). So the store is exempted by
    // relative-path PREFIX over that directory — still anchored at `src/`, so
    // a same-named file elsewhere is not covered.
    const OWNS_THE_PAIRING: &[&str] = &[
        "session/session_lifecycle_store.rs",
        "session/snapshot_history.rs",
    ];
    const OWNS_THE_PAIRING_DIRS: &[&str] = &["session/session_lifecycle_store/"];
    for path in &files {
        let rel = path
            .strip_prefix(&src)
            .unwrap_or_else(|e| panic!("{} is not under {}: {e}", path.display(), src.display()))
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        assert!(
            !body.contains(DELETED_HELPER),
            "{rel} references the DELETED port-keyed reader helper ({DELETED_HELPER}) — \
             it WAS the read/write divergence; resolve through snapshot_history_path()"
        );
        if OWNS_THE_PAIRING.contains(&rel.as_str())
            || OWNS_THE_PAIRING_DIRS.iter().any(|dir| rel.starts_with(dir))
        {
            continue;
        }
        assert!(
            !(body.contains("session_restore_dir") && body.contains("session-snapshots")),
            "{rel} mentions BOTH `session_restore_dir` and `session-snapshots`, which is \
             how the Phase 4 divergence was written: the hook dir is deliberately UNSCOPED \
             (it is the Claude SessionStart materialization target), so a snapshot path \
             built from it points at a file nothing writes on every secondary. Resolve \
             through session_lifecycle_store::snapshot_history_path() instead."
        );
    }
}

/// Recursively collect `*.rs` under `dir` (test helper for the grep-shaped
/// guard above).
///
/// PANICS on any walk error rather than skipping the subtree. Returning
/// early on `Err` would drop a whole directory from the scan while the
/// caller's file-count floor still passed — the guard would report green
/// having examined less than it thinks (`verification-and-evidence`
/// `silent-empty-is-unknown`). A Windows long-path or transient permission
/// error on `src/mcp/` alone would hide 152 files.
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("walking {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("reading an entry of {}: {e}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn rec(id: &str) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: id.to_string(),
        config_dir: Some("C:/cfg".to_string()),
        working_dir: Some("C:/repo".to_string()),
        page_id: "default".to_string(),
        zone_index: 2,
        title: Some("Claude 1".to_string()),
        terminal_id: "term-abc".to_string(),
        // These get overwritten by record_open; seed sane values.
        opened_at: 0,
        last_seen_at: 0,
        state: "open".to_string(),
        closed_at: None,
        close_reason: None,
        provider: DEFAULT_PROVIDER.to_string(),
        origin: None,
        restore_pending_at: None,
        awaiting_account_since: None,
        confirmed_at: None,
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
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
    }
}

/// Age fields on the persisted registry that no public mutator can reach
/// (`closed_at`, `last_seen_at`), the WAL-model-correct way.
///
/// Under the write-ahead log a mutation lands as ONE appended delta and the
/// JSON snapshot is only rewritten on compaction, so the pre-WAL idiom —
/// read the snapshot, edit it, write it back — read a stale (or, on a store
/// that has never compacted, an absent) file and then had the un-folded
/// deltas replayed straight back over the edit on the next `open`.
/// [`SessionLifecycleStore::compact`] first makes the snapshot the
/// authoritative copy and truncates the WAL, so the edit is the last word:
/// reopening the path afterwards loads exactly the tampered map.
///
/// Callers must reopen the store afterwards — `store`'s in-memory map is
/// deliberately NOT updated, matching the "aged on disk, then restarted"
/// scenario every caller is reproducing.
fn age_persisted_records(
    store: &SessionLifecycleStore,
    path: &Path,
    edit: impl FnOnce(&mut HashMap<String, TerminalSessionRecord>),
) {
    store.compact();
    let raw = std::fs::read(path).unwrap();
    let mut m: HashMap<String, TerminalSessionRecord> = serde_json::from_slice(&raw).unwrap();
    edit(&mut m);
    std::fs::write(path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
}

// ── Write-ahead log (Phase 6, B1) ───────────────────────────────────────
//
// A mutation costs ONE appended delta line instead of a whole-map JSON
// rewrite. These prove the three properties that buys us nothing without:
// deltas survive a reopen, a torn tail loses only the uncommitted line, and
// compaction is idempotent against replay.

fn sdd(tenant: Option<&str>) -> SpawnDeviceDefault {
    SpawnDeviceDefault {
        tenant_id: tenant.map(str::to_string),
        unresolvable: false,
    }
}

/// W-B (plan 2026-09-10-spawn-tenant-never-reaches-the-session-coord-credential):
/// the spawn-time device default is written onto a record THIS spawn created,
/// survives a reopen (a runner restart), and is never replaced — not by a
/// second stamp, not by a re-record, and not by a restore re-spawn whose
/// record pre-dates it.
#[test]
fn the_spawn_device_default_is_recorded_once_and_survives_a_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        let before_spawn = Utc::now().timestamp_millis();
        store.record_open(rec("sess-spawn"));
        assert!(store.record_spawn_device_default("term-abc", sdd(Some("tenant-a")), before_spawn));
        // A later stamp (a restore-time default) never overwrites it.
        assert!(!store.record_spawn_device_default(
            "term-abc",
            sdd(Some("tenant-b")),
            before_spawn
        ));
        // Nor does a re-record carrying nothing.
        store.record_open(rec("sess-spawn"));
    }
    let reopened = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        reopened.get("sess-spawn").unwrap().spawn_device_default,
        Some(sdd(Some("tenant-a"))),
        "the value of the SPAWN must survive a restart"
    );
}

/// A record that existed BEFORE the spawn (a resume / boot restore of an
/// older session, possibly one that predates the field) is not stamped with
/// the default read now: that is the restore-time default, and it stays
/// unrecorded rather than wrong.
#[test]
fn a_restore_re_spawn_never_stamps_a_pre_existing_record() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    store.record_open(rec("sess-old"));
    let spawn_after_the_record = Utc::now().timestamp_millis() + 60_000;
    assert!(!store.record_spawn_device_default(
        "term-abc",
        sdd(Some("tenant-restore-time")),
        spawn_after_the_record
    ));
    assert_eq!(store.get("sess-old").unwrap().spawn_device_default, None);
}

/// The row the tenancy report reads is often NOT the seam's provisional row
/// but the one an account launcher's typed `claude --session-id` creates on
/// the same terminal. A brand-new record inherits the terminal's recorded
/// spawn default; a record that already existed does not.
#[test]
fn a_new_record_on_the_terminal_inherits_the_recorded_spawn_default() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    let before = Utc::now().timestamp_millis();
    store.record_open(rec("seam-row"));
    assert!(store.record_spawn_device_default("term-abc", sdd(None), before));
    store.record_open(rec("launcher-row"));
    assert_eq!(
        store.get("launcher-row").unwrap().spawn_device_default,
        Some(sdd(None)),
        "recorded-with-no-default is inherited as recorded, not as absent"
    );

    let mut elsewhere = rec("older-session");
    elsewhere.terminal_id = "term-other".to_string();
    store.record_open(elsewhere.clone());
    elsewhere.terminal_id = "term-abc".to_string();
    store.record_open(elsewhere);
    assert_eq!(
        store.get("older-session").unwrap().spawn_device_default,
        None,
        "a pre-existing record re-bound to the terminal is a resume, not this spawn"
    );
}

/// A mutation is durable via the WAL alone — no compaction needed — and the
/// JSON snapshot has NOT been rewritten (that is the whole O(1) point).
#[test]
fn wal_makes_a_mutation_durable_without_rewriting_the_snapshot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);

    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-wal"));
        assert!(wal.exists(), "the mutation must have landed in the WAL");
        let snapshot = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !snapshot.contains("sess-wal"),
            "the whole-map snapshot must NOT be rewritten per mutation — that is the \
             O(total sessions) cost this replaces (snapshot was: {snapshot})"
        );
    }

    let reopened = SessionLifecycleStore::open(&path).unwrap();
    assert!(
        reopened.get("sess-wal").is_some(),
        "a WAL-only mutation must survive a reopen"
    );
}

/// Every mutation kind round-trips through the WAL, including the two that
/// DELETE (`remove_session`, `rekey_session`) — a replay that only ever
/// upserts would resurrect them.
#[test]
fn wal_replays_upserts_removals_and_rekeys_in_order() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("keep"));
        store.record_open(rec("drop"));
        store.record_open(rec("old-id"));
        store.remove_session("drop");
        store.rekey_session("old-id", "new-id");
        store.update_title_by_terminal("term-abc", "renamed");
    }
    let reopened = SessionLifecycleStore::open(&path).unwrap();
    assert!(reopened.get("keep").is_some());
    assert!(
        reopened.get("drop").is_none(),
        "a Remove delta must survive replay"
    );
    assert!(reopened.get("old-id").is_none(), "the rekey source is gone");
    assert!(
        reopened.get("new-id").is_some(),
        "the rekey target is there"
    );
}

/// CRASH SAFETY. A process killed mid-append leaves a partial final line.
/// Every line before it is committed and must be replayed; the partial one
/// was never committed and must be dropped — not fatal, and not silently
/// half-parsed.
#[test]
fn torn_wal_tail_drops_only_the_uncommitted_line() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);
    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("committed-1"));
        let mut r = rec("committed-2");
        r.terminal_id = "term-two".to_string();
        store.record_open(r);
    }
    // Append a HALF-written third delta: valid prefix, no newline.
    let torn = serde_json::to_string(&LifecycleDelta::Upsert {
        rec: Box::new(rec("torn")),
    })
    .unwrap();
    let mut f = std::fs::OpenOptions::new().append(true).open(&wal).unwrap();
    f.write_all(&torn.as_bytes()[..torn.len() / 2]).unwrap();
    drop(f);

    let reopened = SessionLifecycleStore::open(&path).unwrap();
    assert!(
        reopened.get("committed-1").is_some() && reopened.get("committed-2").is_some(),
        "a torn tail must not cost the committed records before it"
    );
    assert!(
        reopened.get("torn").is_none(),
        "an unterminated tail line was never committed and must be dropped"
    );
    // The damaged log is folded into a clean snapshot on open, so the NEXT
    // append can never be concatenated onto the partial line.
    assert_eq!(
        std::fs::read_to_string(&wal).unwrap(),
        "",
        "a damaged WAL must be compacted away at open"
    );
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("committed-1"));
}

/// A complete-but-unparsable delta (schema drift, media corruption) costs
/// only itself — the same fail-soft rule `load_map` applies to the snapshot.
#[test]
fn malformed_wal_line_drops_only_that_delta() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);
    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("before"));
    }
    {
        let mut f = std::fs::OpenOptions::new().append(true).open(&wal).unwrap();
        writeln!(f, "{{\"op\":\"upsert\",\"rec\":\"not a record\"}}").unwrap();
        let mut r = rec("after");
        r.terminal_id = "term-after".to_string();
        let good = serde_json::to_string(&LifecycleDelta::Upsert { rec: Box::new(r) }).unwrap();
        writeln!(f, "{good}").unwrap();
    }
    let reopened = SessionLifecycleStore::open(&path).unwrap();
    assert!(reopened.get("before").is_some());
    assert!(
        reopened.get("after").is_some(),
        "a bad line must not abort the replay of the good ones after it"
    );
}

/// Compaction publishes the snapshot BEFORE truncating the WAL, so the
/// crash window replays deltas the snapshot already holds. Replay is
/// last-writer-wins per key, so that is a no-op — prove it.
#[test]
fn replaying_a_wal_the_snapshot_already_contains_is_idempotent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);
    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-x"));
        let before = std::fs::read_to_string(&wal).unwrap();
        store.compact();
        // Simulate the crash window: snapshot written + renamed, WAL not
        // yet truncated.
        std::fs::write(&wal, before).unwrap();
    }
    let reopened = SessionLifecycleStore::open(&path).unwrap();
    let all = reopened.all_records();
    assert_eq!(all.len(), 1, "a replayed-twice record must not duplicate");
    assert_eq!(all[0].claude_session_id, "sess-x");
}

/// The WAL is bounded: past [`WAL_COMPACT_APPENDS`] the store folds it into
/// the snapshot on its own, so boot replay cost stays bounded no matter how
/// long the runner has been up.
#[test]
fn wal_compacts_itself_past_the_append_bound() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-touch"));
    for _ in 0..WAL_COMPACT_APPENDS {
        store.touch("sess-touch");
    }
    assert!(
        std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) < 4096,
        "the WAL must have been folded into the snapshot, not grown without bound"
    );
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("sess-touch"));
}

/// Idle compaction: the liveness poll's heartbeat folds a dirty WAL into
/// the snapshot, so an idle runner converges without any mutation paying an
/// O(N) rewrite.
#[test]
fn heartbeat_folds_a_dirty_wal_into_the_snapshot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-hb"));
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    store.snapshot_heartbeat();
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    assert!(std::fs::read_to_string(&path).unwrap().contains("sess-hb"));
}

/// The `terminal_id -> claude_session_id` reverse index that the enriched
/// `terminal_list` (`sessionIdsByTerminal`) relies on to re-attach
/// `claudeSessionId` to reconnected tabs: an OPEN record resolves by its
/// terminal id (carrying the session id + config dir), and an unknown
/// terminal resolves to `None`.
#[test]
fn find_open_by_terminal_resolves_session_id_for_reconnect() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(&dir.path().join("terminal-sessions.json")).unwrap();

    let mut r = rec("sess-1");
    r.terminal_id = "term-1".to_string();
    store.record_open(r);

    let found = store
        .find_open_by_terminal("term-1")
        .expect("open record resolvable by its terminal id");
    assert_eq!(found.claude_session_id, "sess-1");
    assert_eq!(found.config_dir.as_deref(), Some("C:/cfg"));

    assert!(
        store.find_open_by_terminal("term-unknown").is_none(),
        "an unknown terminal id resolves to None"
    );
}

/// `find_confirmed_open_by_terminal` — the gate `terminal_list`'s
/// `sessionIdsByTerminal` uses — returns a record ONLY once it is confirmed.
/// The spawn-time identity seam records an authoritative-but-PROVISIONAL
/// `open` row for every terminal (plain shells included, and non-pinned
/// launches whose minted uuid is never the run id); surfacing those would
/// bind phantom / wrong ids onto tabs. Provisional ⇒ `None`, confirmed ⇒
/// `Some`, while the unfiltered `find_open_by_terminal` still resolves both.
#[test]
fn find_confirmed_open_by_terminal_excludes_provisional_phantoms() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(&dir.path().join("terminal-sessions.json")).unwrap();

    // A provisional (unconfirmed) row — the phantom-shell / minted-uuid shape.
    let mut phantom = rec("phantom-sess");
    phantom.terminal_id = "term-phantom".to_string();
    phantom.confirmed_at = None;
    store.record_open(phantom);

    assert!(
        store
            .find_confirmed_open_by_terminal("term-phantom")
            .is_none(),
        "an unconfirmed (provisional) record is NOT surfaced to session-scoped UI"
    );
    assert!(
        store.find_open_by_terminal("term-phantom").is_some(),
        "the unfiltered lookup still resolves the provisional record"
    );

    // A confirmed row — a real session with a fired hook / start-anchored bind.
    let mut real = rec("real-sess");
    real.terminal_id = "term-real".to_string();
    real.confirmed_at = Some(1_700_000_000_000);
    store.record_open(real);

    let found = store
        .find_confirmed_open_by_terminal("term-real")
        .expect("a confirmed record resolves by its terminal id");
    assert_eq!(found.claude_session_id, "real-sess");
    assert_eq!(found.config_dir.as_deref(), Some("C:/cfg"));

    assert!(
        store
            .find_confirmed_open_by_terminal("term-unknown")
            .is_none(),
        "an unknown terminal id resolves to None"
    );
}

/// A pane pinned to P whose operator then ran `/resume X` keeps BOTH rows
/// open (the hook bind evicts only unconfirmed siblings), and the terminal
/// lookups must name X — the session actually running — every time, not
/// whichever row the `HashMap` yields first. Incident 2026-10-29: the
/// token-exhaustion migration resolved terminal `5b27af82` to its stale
/// pane session, resumed that, and closed the pane running the real one.
/// Driven through the same calls `POST /control/session-open` makes.
#[test]
fn terminal_lookups_name_the_resumed_session_not_the_stale_pane_row() {
    for _ in 0..32 {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        for id in ["pane-p", "resumed-x"] {
            store.record_open(auth_on(id, "term-r"));
            store.confirm_session(id);
            store.note_session_start_hook(id);
        }
        assert_eq!(
            store.get("pane-p").unwrap().state,
            "open",
            "precondition: the stale pane row survives the resume"
        );
        // An unconfirmed row bound later must not displace X from the
        // confirmed lookup.
        let mut phantom = rec("phantom");
        phantom.terminal_id = "term-r".to_string();
        phantom.origin = Some(ORIGIN_OBSERVED.to_string());
        store.record_open(phantom);

        assert_eq!(
            store
                .find_open_by_terminal("term-r")
                .unwrap()
                .claude_session_id,
            "resumed-x"
        );
        assert_eq!(
            store
                .find_confirmed_open_by_terminal("term-r")
                .unwrap()
                .claude_session_id,
            "resumed-x"
        );
    }
}

/// The same answer after a restart: the process-local hook order is gone
/// (a holder-adopted pane fires no new hook), so the durable
/// `last_session_start_at` must carry it. The resumed session is given the
/// OLDER `confirmed_at` — it was confirmed in another pane before this one
/// — which is exactly the case `confirmed_at` alone gets wrong.
#[test]
fn resumed_session_still_wins_after_reload() {
    for _ in 0..32 {
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.json");
        {
            let store = SessionLifecycleStore::open(&path).unwrap();
            let mut x = auth_on("resumed-x", "term-r");
            x.confirmed_at = Some(1_000);
            store.record_open(x);
            store.record_open(auth_on("pane-p", "term-r"));
            store.confirm_session("pane-p");
            store.note_session_start_hook("pane-p");
            std::thread::sleep(std::time::Duration::from_millis(2));
            store.note_session_start_hook("resumed-x");
            assert_eq!(store.get("pane-p").unwrap().state, "open");
        }
        let reloaded = SessionLifecycleStore::open(&path).unwrap();
        assert!(reloaded
            .get("resumed-x")
            .unwrap()
            .last_session_start_at
            .is_some());
        assert_eq!(
            reloaded
                .find_open_by_terminal("term-r")
                .unwrap()
                .claude_session_id,
            "resumed-x"
        );
    }
}

/// A page move relocates every open row on the pane, so the stale sibling
/// cannot be restored on the page the pane left.
#[test]
fn update_page_by_terminal_moves_every_open_row_on_the_pane() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    for id in ["pane-p", "resumed-x"] {
        store.record_open(auth_on(id, "term-r"));
        store.confirm_session(id);
    }
    store.record_open(auth_on("elsewhere", "term-other"));
    store.update_page_by_terminal("term-r", "page-2");
    assert_eq!(store.get("pane-p").unwrap().page_id, "page-2");
    assert_eq!(store.get("resumed-x").unwrap().page_id, "page-2");
    assert_eq!(store.get("elsewhere").unwrap().page_id, "default");
}

/// `confirm_session` flips provisional→confirmed monotonically, a
/// provisional re-record never clears it, and pre-Phase-2 rows load
/// provisional (session-restore-redesign Phase 2 coordinator refinement).
#[test]
fn confirm_session_flips_provisional_and_record_open_never_clears_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // Spawn-time write is PROVISIONAL (confirmed_at unset).
    store.record_open(rec("sess-1"));
    assert!(
        store.get("sess-1").unwrap().confirmed_at.is_none(),
        "spawn-time record is provisional"
    );

    // The provider's SessionStart hook fires → confirm.
    store.confirm_session("sess-1");
    let confirmed_at = store.get("sess-1").unwrap().confirmed_at;
    assert!(confirmed_at.is_some(), "hook flips it to confirmed");

    // Confirmation is monotonic: a second confirm (a later resume hook) is a
    // no-op (does NOT bump the timestamp).
    store.confirm_session("sess-1");
    assert_eq!(
        store.get("sess-1").unwrap().confirmed_at,
        confirmed_at,
        "second confirm is a no-op (first hook wins)"
    );

    // A PROVISIONAL re-record (spawn-time writer / zone-move backstop) must
    // NOT clear an existing confirmation.
    store.record_open(rec("sess-1"));
    assert_eq!(
        store.get("sess-1").unwrap().confirmed_at,
        confirmed_at,
        "provisional re-record preserves confirmation"
    );

    // Durable across reload + confirming an absent id is a no-op.
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.get("sess-1").unwrap().confirmed_at.is_some());
    store.confirm_session("ghost"); // no panic

    // A pre-Phase-2 on-disk record (no confirmedAt key) loads provisional.
    let json = r#"{"old": {
            "claudeSessionId":"old","configDir":null,"workingDir":"C:/repo",
            "pageId":"default","zoneIndex":1,"title":"Old","terminalId":"t",
            "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,"closeReason":null
        }}"#;
    let p2 = dir.path().join("legacy.json");
    std::fs::write(&p2, json).unwrap();
    let s2 = SessionLifecycleStore::open(&p2).unwrap();
    assert!(
        s2.get("old").unwrap().confirmed_at.is_none(),
        "pre-Phase-2 record loads provisional"
    );
}

/// Session-identity fabric Phase 1 — the `fsh_` handle round-trips through
/// disk, is STICKY across a handle-less re-record, only writes on change,
/// the server wins on divergence, `set_handle` never creates records, and
/// a LEGACY on-disk row without the field loads cleanly as `None`.
#[test]
fn handle_persists_sticky_and_legacy_rows_load_without_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("sess-h"));
    assert!(
        store.get("sess-h").unwrap().handle.is_none(),
        "no handle until acquired"
    );

    // Acquire → persisted.
    store.set_handle("sess-h", "fsh_abc");
    assert_eq!(
        store.get("sess-h").unwrap().handle.as_deref(),
        Some("fsh_abc")
    );

    // A handle-less re-record (zone-move backstop / boot re-assert /
    // provider hook) must NOT clear it — sticky like config_dir.
    store.record_open(rec("sess-h"));
    assert_eq!(
        store.get("sess-h").unwrap().handle.as_deref(),
        Some("fsh_abc"),
        "handle-less re-record preserves the handle"
    );

    // Same value re-set is a no-op; a DIVERGENT server value overwrites
    // (server wins — rebind is keyed on claude_session_id server-side).
    store.set_handle("sess-h", "fsh_abc");
    store.set_handle("sess-h", "fsh_new");
    assert_eq!(
        store.get("sess-h").unwrap().handle.as_deref(),
        Some("fsh_new")
    );

    // set_handle on an absent id never creates a record (a handle-only
    // row would be a phantom restore candidate).
    store.set_handle("ghost", "fsh_ghost");
    assert!(store.get("ghost").is_none());

    // Durable across reload, and a record_open CARRYING a handle sets it.
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        store.get("sess-h").unwrap().handle.as_deref(),
        Some("fsh_new"),
        "handle survives a reload"
    );
    let mut r = rec("sess-h2");
    r.handle = Some("fsh_h2".to_string());
    store.record_open(r);
    assert_eq!(
        store.get("sess-h2").unwrap().handle.as_deref(),
        Some("fsh_h2")
    );

    // A legacy on-disk row without the `handle` key loads cleanly as None
    // (serde default — the fabric field is purely additive).
    let json = r#"{"old": {
            "claudeSessionId":"old","configDir":null,"workingDir":"C:/repo",
            "pageId":"default","zoneIndex":1,"title":"Old","terminalId":"t",
            "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,"closeReason":null
        }}"#;
    let p2 = dir.path().join("legacy-handle.json");
    std::fs::write(&p2, json).unwrap();
    let s2 = SessionLifecycleStore::open(&p2).unwrap();
    assert!(
        s2.get("old").unwrap().handle.is_none(),
        "legacy row without the field loads with no handle"
    );
}

/// `config_dir` is STICKY and empty-normalized (G2 — account-correct
/// restore): a known-good account binding survives a later `record_open`
/// that omits the account (a provider that doesn't report it, a zone-move
/// backstop, Gemini), a stray empty/whitespace incoming normalizes to
/// `None` (never a bogus `CLAUDE_CONFIG_DIR=""` on resume), and a later
/// non-empty `Some` still updates (sticky ≠ frozen).
#[test]
fn record_open_config_dir_is_sticky_and_empty_normalizes_to_none() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // Bind a known-good account.
    let mut r = rec("sess-cfg");
    r.config_dir = Some("C:/cfg".to_string());
    store.record_open(r);
    assert_eq!(
        store.get("sess-cfg").unwrap().config_dir.as_deref(),
        Some("C:/cfg")
    );

    // A later write that OMITS config_dir (None) must NOT clobber it.
    let mut r2 = rec("sess-cfg");
    r2.config_dir = None;
    store.record_open(r2);
    assert_eq!(
        store.get("sess-cfg").unwrap().config_dir.as_deref(),
        Some("C:/cfg"),
        "a None re-record must preserve the known account binding"
    );

    // An empty/whitespace-only incoming normalizes to None → also preserves.
    let mut r3 = rec("sess-cfg");
    r3.config_dir = Some("   ".to_string());
    store.record_open(r3);
    assert_eq!(
        store.get("sess-cfg").unwrap().config_dir.as_deref(),
        Some("C:/cfg"),
        "an empty incoming normalizes to None and preserves the binding"
    );

    // A brand-new record whose only config_dir is empty stores None, never "".
    let mut r4 = rec("sess-empty");
    r4.config_dir = Some(String::new());
    store.record_open(r4);
    assert!(
        store.get("sess-empty").unwrap().config_dir.is_none(),
        "empty config_dir normalizes to None on a fresh record"
    );

    // A later non-empty Some DOES update (sticky is not frozen).
    let mut r5 = rec("sess-cfg");
    r5.config_dir = Some("C:/other".to_string());
    store.record_open(r5);
    assert_eq!(
        store.get("sess-cfg").unwrap().config_dir.as_deref(),
        Some("C:/other"),
        "a later non-empty Some updates the binding"
    );
}

// ---------------------------------------------------------------------
// D1 — durable session-info fields (account, name, tenant, restore tier)
// ---------------------------------------------------------------------

/// A LEGACY on-disk record carrying none of the D1 fields must load
/// cleanly, with every one of them `None`.
///
/// This is the whole claim of the additive `#[serde(default)]` migration:
/// the on-disk format did not change, so a registry written by any prior
/// build restores exactly as before. A regression here silently wipes every
/// restorable on-screen session at boot (`load_map` drops rows it cannot
/// deserialize), which is why it gets its own test rather than riding along
/// with the behavioural ones.
#[test]
fn legacy_record_loads_with_every_session_info_field_none() {
    let json = r#"{"claudeSessionId":"legacy-sess","configDir":"C:/cfg",
            "workingDir":"C:/repo","pageId":"default","zoneIndex":1,
            "title":"Old","terminalId":"term-old","openedAt":1,"lastSeenAt":2,
            "state":"open","closedAt":null,"closeReason":null}"#;
    let rec: TerminalSessionRecord = serde_json::from_str(json).unwrap();

    assert_eq!(rec.claude_session_id, "legacy-sess");
    assert!(
        rec.account_label.is_none(),
        "no account label on a legacy row"
    );
    assert!(rec.account_wrapper.is_none());
    assert!(rec.session_name.is_none());
    assert!(rec.name_source.is_none());
    assert!(rec.tenant_id.is_none());
    assert!(rec.task_run_id.is_none());
    assert!(rec.bypass_permissions.is_none());
    assert!(rec.restored_from_boot_at.is_none());
    assert!(
        rec.restore_tier.is_none(),
        "never-restored and restore-failed must stay distinguishable"
    );

    // And the same row loads through the real store path, not just serde.
    let dir = tempdir().unwrap();
    let path = dir.path().join("legacy-session-info.json");
    std::fs::write(&path, format!(r#"{{"legacy-sess": {json} }}"#)).unwrap();
    let store = SessionLifecycleStore::open(&path).unwrap();
    let loaded = store
        .get("legacy-sess")
        .expect("legacy row must survive load");
    assert!(loaded.account_label.is_none());
    assert!(loaded.session_name.is_none());

    // Absent fields are also absent on the way back OUT
    // (`skip_serializing_if`), so a legacy row does not grow nine null keys
    // just by being loaded and re-persisted.
    let round = serde_json::to_string(&loaded).unwrap();
    for key in [
        "accountLabel",
        "accountWrapper",
        "sessionName",
        "nameSource",
        "tenantId",
        "taskRunId",
        "bypassPermissions",
        "restoredFromBootAt",
        "restoreTier",
    ] {
        assert!(
            !round.contains(key),
            "absent {key} must not be serialized (got: {round})"
        );
    }
}

/// THE stickiness rule: a live-registry read that reports nothing must
/// never blank a stored account or name — absent ⇒ keep.
///
/// Mirrors `record_open_config_dir_is_sticky_and_empty_normalizes_to_none`,
/// because it is the same rule: the sources for these fields (the live
/// per-process registry, the provider hook, the spawn writer) each know
/// only part of the answer and are absent far more often than they are
/// wrong. Both write paths are covered — `record_open` and the
/// `update_identity` overlay — since a hole in either one loses the data.
#[test]
fn session_identity_is_sticky_across_record_open_and_update_identity() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // Spawn stamps the account; the poll later stamps the name.
    let mut r = rec("sess-id");
    r.account_label = Some("gmail".to_string());
    r.account_wrapper = Some("clg".to_string());
    store.record_open(r);
    store.update_identity(
        "sess-id",
        &SessionIdentityUpdate {
            session_name: Some("coord merge train".to_string()),
            name_source: Some(NAME_SOURCE_OPERATOR.to_string()),
            ..Default::default()
        },
    );
    let got = store.get("sess-id").unwrap();
    assert_eq!(got.account_label.as_deref(), Some("gmail"));
    assert_eq!(got.session_name.as_deref(), Some("coord merge train"));

    // A re-record that omits everything (zone move, boot re-assert, a
    // provider that reports no account) must preserve all of it.
    store.record_open(rec("sess-id"));
    let got = store.get("sess-id").unwrap();
    assert_eq!(
        got.account_label.as_deref(),
        Some("gmail"),
        "a None re-record must preserve the known account binding"
    );
    assert_eq!(got.account_wrapper.as_deref(), Some("clg"));
    assert_eq!(
        got.session_name.as_deref(),
        Some("coord merge train"),
        "a None re-record must preserve the known session name"
    );
    assert_eq!(got.name_source.as_deref(), Some(NAME_SOURCE_OPERATOR));

    // An all-empty overlay is a no-op, not a blanking write — this is the
    // shape a failed/partial live-registry read produces.
    assert!(
        !store.update_identity("sess-id", &SessionIdentityUpdate::default()),
        "an empty overlay must not write"
    );
    // Blank strings degrade to "not reported", never to a destructive write.
    assert!(!store.update_identity(
        "sess-id",
        &SessionIdentityUpdate {
            account_label: Some("   ".to_string()),
            session_name: Some(String::new()),
            ..Default::default()
        },
    ));
    let got = store.get("sess-id").unwrap();
    assert_eq!(got.account_label.as_deref(), Some("gmail"));
    assert_eq!(got.session_name.as_deref(), Some("coord merge train"));

    // Sticky is not FROZEN: a real new value still wins (an operator
    // `/rename` must be able to displace Claude's auto-name).
    store.update_identity(
        "sess-id",
        &SessionIdentityUpdate {
            session_name: Some("renamed by operator".to_string()),
            ..Default::default()
        },
    );
    assert_eq!(
        store.get("sess-id").unwrap().session_name.as_deref(),
        Some("renamed by operator")
    );
    // An unchanged overlay costs no durable write.
    assert!(!store.update_identity(
        "sess-id",
        &SessionIdentityUpdate {
            session_name: Some("renamed by operator".to_string()),
            ..Default::default()
        },
    ));

    // Never creates a record: an identity-only row would be a phantom
    // restore candidate.
    assert!(!store.update_identity(
        "ghost",
        &SessionIdentityUpdate {
            session_name: Some("ghost".to_string()),
            ..Default::default()
        },
    ));
    assert!(store.get("ghost").is_none());

    // The restore markers are sticky the same way — a re-record must not
    // erase the census's only evidence that this row was re-materialized.
    store.mark_restored_from_boot("sess-id", RESTORE_TIER_RESUMED);
    store.record_open(rec("sess-id"));
    let got = store.get("sess-id").unwrap();
    assert_eq!(got.restore_tier.as_deref(), Some(RESTORE_TIER_RESUMED));
    assert!(got.restored_from_boot_at.is_some());
    // A tier can still be corrected (a resume that later fails).
    store.mark_restored_from_boot("sess-id", RESTORE_TIER_FAILED);
    assert_eq!(
        store.get("sess-id").unwrap().restore_tier.as_deref(),
        Some(RESTORE_TIER_FAILED)
    );
    store.mark_restored_from_boot("ghost", RESTORE_TIER_TERMINAL_ONLY);
    assert!(
        store.get("ghost").is_none(),
        "restore marker never creates a row"
    );
}

/// Every D1 field survives the JSON+WAL durability path — a mutation lands
/// as an appended delta, is replayed on reopen, and still reads back after
/// a compaction folds it into the snapshot.
///
/// The two halves matter separately: the WAL leg proves an identity write
/// is durable WITHOUT a whole-map rewrite (which is what makes it cheap
/// enough to run on a 45s poll tick), and the compaction leg proves the
/// fields survive the snapshot serialization too.
#[test]
fn session_info_fields_round_trip_through_the_wal_and_a_compaction() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let wal = wal_path_for(&path);

    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        let mut r = rec("sess-rt");
        r.account_label = Some("paktis".to_string());
        r.account_wrapper = Some("clp".to_string());
        r.tenant_id = Some("11111111-2222-3333-4444-555555555555".to_string());
        r.task_run_id = Some("run-77".to_string());
        r.bypass_permissions = Some(true);
        store.record_open(r);
        store.update_identity(
            "sess-rt",
            &SessionIdentityUpdate {
                session_name: Some("wal round trip".to_string()),
                name_source: Some(NAME_SOURCE_DERIVED.to_string()),
                ..Default::default()
            },
        );
        store.mark_restored_from_boot("sess-rt", RESTORE_TIER_TERMINAL_ONLY);

        assert!(wal.exists(), "the mutations must have landed in the WAL");
        let snapshot = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !snapshot.contains("wal round trip"),
            "an identity write must not force a whole-map rewrite (snapshot: {snapshot})"
        );
    }

    // Reopen replays the WAL.
    let reopened = SessionLifecycleStore::open(&path).unwrap();
    let got = reopened
        .get("sess-rt")
        .expect("WAL replay must restore the record");
    assert_eq!(got.account_label.as_deref(), Some("paktis"));
    assert_eq!(got.account_wrapper.as_deref(), Some("clp"));
    assert_eq!(got.session_name.as_deref(), Some("wal round trip"));
    assert_eq!(got.name_source.as_deref(), Some(NAME_SOURCE_DERIVED));
    assert_eq!(
        got.tenant_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
    assert_eq!(got.task_run_id.as_deref(), Some("run-77"));
    assert_eq!(got.bypass_permissions, Some(true));
    assert_eq!(
        got.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY)
    );
    let restored_at = got.restored_from_boot_at.expect("restore marker persisted");
    assert!(restored_at > 0);

    // And again after compaction folds the deltas into the JSON snapshot.
    reopened.compact();
    let snapshot = std::fs::read_to_string(&path).unwrap();
    assert!(snapshot.contains("wal round trip"));
    assert!(snapshot.contains("accountWrapper"), "camelCase on disk");
    let compacted = SessionLifecycleStore::open(&path).unwrap();
    let got = compacted.get("sess-rt").unwrap();
    assert_eq!(got.session_name.as_deref(), Some("wal round trip"));
    assert_eq!(got.restored_from_boot_at, Some(restored_at));
    assert_eq!(got.bypass_permissions, Some(true));
}

/// `durable_name_source` resolves the registry's ABSENT-means-operator
/// convention at observation time, so a stored `None` keeps its one honest
/// meaning ("never observed") and a `/rename` can displace `"derived"`.
#[test]
fn durable_name_source_resolves_absence_to_operator_and_passes_others_through() {
    assert_eq!(durable_name_source(None), NAME_SOURCE_OPERATOR);
    assert_eq!(durable_name_source(Some("  ")), NAME_SOURCE_OPERATOR);
    assert_eq!(durable_name_source(Some("derived")), NAME_SOURCE_DERIVED);
    // A future CLI variant is not this function's to interpret.
    assert_eq!(durable_name_source(Some("launch-flag")), "launch-flag");
}

/// Records predating the origin field must deserialize and read as
/// reconciled (the conservative default).
#[test]
fn pre_origin_record_deserializes_as_reconciled() {
    let json = r#"{"claudeSessionId":"old-sess","configDir":null,"workingDir":"C:/repo",
            "pageId":"default","zoneIndex":1,"title":"Old","terminalId":"term-old",
            "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,"closeReason":null}"#;
    let rec: TerminalSessionRecord = serde_json::from_str(json).unwrap();
    // `None` IS the reconciled reading — consumers default absent to
    // "reconciled". Provider defaults to claude for pre-provider rows.
    assert_eq!(
        rec.origin.as_deref().unwrap_or(ORIGIN_RECONCILED),
        ORIGIN_RECONCILED
    );
    assert_eq!(rec.provider, DEFAULT_PROVIDER);
}

/// A legacy on-disk record carrying `bindOrigin:"pinned"` must load as
/// `origin:"authoritative"`, and `bindOrigin:"guessed"` as
/// `origin:"reconciled"` (the value migration, exercised through the real
/// load path so the `alias` + `load_map` normalization both fire).
#[test]
fn legacy_bind_origin_values_migrate_on_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let file = r#"{
            "pin": {
                "claudeSessionId":"pin","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":1,"title":"P","terminalId":"t1",
                "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,
                "closeReason":null,"bindOrigin":"pinned"
            },
            "guess": {
                "claudeSessionId":"guess","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":2,"title":"G","terminalId":"t2",
                "openedAt":1,"lastSeenAt":2,"state":"open","closedAt":null,
                "closeReason":null,"bindOrigin":"guessed"
            }
        }"#;
    std::fs::write(&path, file).unwrap();
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        store.get("pin").unwrap().origin.as_deref(),
        Some(ORIGIN_AUTHORITATIVE),
        "legacy pinned migrates to authoritative"
    );
    assert_eq!(
        store.get("guess").unwrap().origin.as_deref(),
        Some(ORIGIN_RECONCILED),
        "legacy guessed migrates to reconciled"
    );
    // Pre-provider rows default to claude.
    assert_eq!(store.get("pin").unwrap().provider, DEFAULT_PROVIDER);
}

/// Authoritative origin survives an unasserted re-record; asserted updates.
#[test]
fn record_open_preserves_origin_when_unasserted() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    let mut auth = rec("sess-1");
    auth.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
    store.record_open(auth);
    store.record_open(rec("sess-1")); // unasserted (None)
    assert_eq!(
        store.open_records()[0].origin.as_deref(),
        Some(ORIGIN_AUTHORITATIVE)
    );
    let mut reconciled = rec("sess-1");
    reconciled.origin = Some(ORIGIN_RECONCILED.to_string());
    store.record_open(reconciled);
    assert_eq!(
        store.open_records()[0].origin.as_deref(),
        Some(ORIGIN_RECONCILED)
    );
}

/// `record_open` normalizes a legacy origin value handed in by an
/// un-migrated caller (`pinned` → `authoritative`).
#[test]
fn record_open_normalizes_legacy_origin_value() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    let mut legacy = rec("sess-1");
    legacy.origin = Some("pinned".to_string());
    store.record_open(legacy);
    assert_eq!(
        store.open_records()[0].origin.as_deref(),
        Some(ORIGIN_AUTHORITATIVE)
    );
}

/// An authoritative record on the named terminal, unconfirmed by default.
fn auth_on(id: &str, terminal: &str) -> TerminalSessionRecord {
    let mut r = rec(id);
    r.terminal_id = terminal.to_string();
    r.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
    r
}

/// Single-tenant-terminal invariant: a new AUTHORITATIVE session binding a
/// terminal evicts an older UNCONFIRMED phantom sibling on that same
/// terminal — the identity-seam-vs-account-launcher dual-id split, where the
/// seam pins a fresh id at shell spawn and the launcher then types
/// `claude --session-id <its own id>` into that shell.
#[test]
fn record_open_supersedes_unconfirmed_phantom_on_same_terminal() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    store.record_open(auth_on("seam-phantom", "term-1"));
    store.record_open(auth_on("real", "term-1"));

    let phantom = store.get("seam-phantom").unwrap();
    assert_eq!(phantom.state, "closed", "phantom superseded");
    assert_eq!(phantom.close_reason.as_deref(), Some("superseded"));
    assert_eq!(store.get("real").unwrap().state, "open", "real stays open");
    let open: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    assert_eq!(open, vec!["real".to_string()], "only the real session open");
}

/// Supersede guards: a CONFIRMED sibling, a sibling on a DIFFERENT terminal,
/// and a non-authoritative insert all leave the sibling untouched.
#[test]
fn record_open_supersede_guards() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    // (a) A confirmed sibling represents a session that actually started —
    // never superseded here (the exit / poll-dead path retires it).
    let mut confirmed = auth_on("confirmed", "term-a");
    confirmed.confirmed_at = Some(123);
    store.record_open(confirmed);
    store.record_open(auth_on("newer", "term-a"));
    assert_eq!(
        store.get("confirmed").unwrap().state,
        "open",
        "confirmed sibling survives"
    );

    // (b) A sibling on a different terminal is untouched.
    store.record_open(auth_on("other-term", "term-b"));
    store.record_open(auth_on("fresh", "term-c"));
    assert_eq!(
        store.get("other-term").unwrap().state,
        "open",
        "different-terminal sibling survives"
    );

    // (c) A non-authoritative (reconciled) insert never supersedes — it is
    // itself a guess and restore already quarantines it.
    store.record_open(auth_on("phantom2", "term-d"));
    let mut reconciled = rec("guess");
    reconciled.terminal_id = "term-d".to_string();
    reconciled.origin = Some(ORIGIN_RECONCILED.to_string());
    store.record_open(reconciled);
    assert_eq!(
        store.get("phantom2").unwrap().state,
        "open",
        "reconciled insert does not supersede"
    );
}

/// Launch-agnostic supersede: a CONFIRMED `observed` bind (process-anchored
/// unique transcript correlation) evicts an unconfirmed same-terminal
/// sibling, closing the observed-orphan class while the terminal is alive —
/// even though `observed` is not authoritative.
#[test]
fn record_open_observed_confirmed_supersedes_unconfirmed_sibling() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    // Spawn-time seam pin: authoritative but unconfirmed, on term-1.
    store.record_open(auth_on("seam-phantom", "term-1"));

    // The live binder observes the real session on the same terminal and
    // confirms it (a transcript proved it exists).
    let mut observed = rec("observed-real");
    observed.terminal_id = "term-1".to_string();
    observed.origin = Some(ORIGIN_OBSERVED.to_string());
    observed.confirmed_at = Some(999);
    store.record_open(observed);

    let phantom = store.get("seam-phantom").unwrap();
    assert_eq!(phantom.state, "closed", "unconfirmed sibling superseded");
    assert_eq!(phantom.close_reason.as_deref(), Some("superseded"));
    assert_eq!(
        store.get("observed-real").unwrap().state,
        "open",
        "the confirmed observed bind stays open"
    );
}

/// An UNconfirmed `observed` bind is itself only a candidate — it must NOT
/// supersede a sibling (no proof yet a real session owns the terminal).
#[test]
fn record_open_observed_unconfirmed_does_not_supersede() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    store.record_open(auth_on("sibling", "term-2"));

    let mut observed = rec("observed-provisional");
    observed.terminal_id = "term-2".to_string();
    observed.origin = Some(ORIGIN_OBSERVED.to_string());
    // confirmed_at stays None.
    store.record_open(observed);

    assert_eq!(
        store.get("sibling").unwrap().state,
        "open",
        "an unconfirmed observed bind does not supersede its sibling"
    );
}

/// Terminal-reuse invariant (Phase 3 / P4): when a NEW CONFIRMED session
/// binds a terminal, a PRIOR CONFIRMED `open` row on that SAME terminal (a
/// dead prior run whose exit-close never fired) is retired with
/// `close_reason == "superseded-terminal-reuse"`. A confirmed row on a
/// DIFFERENT terminal is untouched.
#[test]
fn record_open_new_confirmed_retires_prior_confirmed_on_reused_terminal() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    // A prior confirmed run on term-reuse (its exit-close never fired).
    let mut prior = auth_on("prior-run", "term-reuse");
    prior.confirmed_at = Some(100);
    store.record_open(prior);

    // A confirmed run on a DIFFERENT terminal — must survive untouched.
    let mut elsewhere = auth_on("other-term-run", "term-other");
    elsewhere.confirmed_at = Some(100);
    store.record_open(elsewhere);

    // A NEW confirmed session binds the SAME reused terminal.
    let mut next = auth_on("next-run", "term-reuse");
    next.confirmed_at = Some(200);
    store.record_open(next);

    let prior = store.get("prior-run").unwrap();
    assert_eq!(prior.state, "closed", "prior confirmed run retired");
    assert_eq!(
        prior.close_reason.as_deref(),
        Some("superseded-terminal-reuse"),
        "retired with the terminal-reuse reason (distinct from the phantom `superseded`)"
    );
    assert_eq!(
        store.get("next-run").unwrap().state,
        "open",
        "the new confirmed session stays open"
    );
    assert_eq!(
        store.get("other-term-run").unwrap().state,
        "open",
        "a confirmed row on a different terminal is untouched"
    );
}

/// The confirmed-reuse arm is gated on the INCOMING being confirmed: an
/// authoritative-but-UNCONFIRMED incoming must NOT retire a confirmed
/// sibling on the same terminal (it is not yet itself proven).
#[test]
fn record_open_unconfirmed_incoming_does_not_retire_confirmed_sibling() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    let mut confirmed = auth_on("confirmed-run", "term-x");
    confirmed.confirmed_at = Some(100);
    store.record_open(confirmed);

    // Authoritative but UNCONFIRMED incoming on the same terminal.
    store.record_open(auth_on("unconfirmed-incoming", "term-x"));

    assert_eq!(
        store.get("confirmed-run").unwrap().state,
        "open",
        "an authoritative-but-unconfirmed incoming must not retire a confirmed sibling"
    );
}

/// Part B: an incoming record with an empty/whitespace-only `terminal_id`
/// must NOT clobber an existing non-empty binding (mirroring the `config_dir`
/// sticky guard); a later non-empty terminal still updates it.
#[test]
fn record_open_empty_terminal_id_does_not_clobber_binding() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    let mut r = rec("sess-term");
    r.terminal_id = "term-real".to_string();
    store.record_open(r);
    assert_eq!(store.get("sess-term").unwrap().terminal_id, "term-real");

    // A later write that omits the terminal (whitespace-only) preserves it.
    let mut r2 = rec("sess-term");
    r2.terminal_id = "   ".to_string();
    store.record_open(r2);
    assert_eq!(
        store.get("sess-term").unwrap().terminal_id,
        "term-real",
        "an empty/whitespace incoming terminal_id preserves the real binding"
    );

    // A later non-empty terminal DOES update (sticky is not frozen).
    let mut r3 = rec("sess-term");
    r3.terminal_id = "term-moved".to_string();
    store.record_open(r3);
    assert_eq!(
        store.get("sess-term").unwrap().terminal_id,
        "term-moved",
        "a later non-empty terminal_id updates the binding"
    );
}

/// Part C: `repair_terminal_id_collisions` collapses N `open` rows sharing
/// one terminal_id down to the NEWEST open (by last_seen_at), closing the
/// rest as `superseded-terminal-reuse`; rows on distinct terminals are
/// untouched; the return value is the number closed.
#[test]
fn repair_terminal_id_collisions_collapses_to_newest_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");

    // Three open rows collided on "shared", plus a distinct-terminal open
    // row and an already-closed row (which must not count).
    let mk = |id: &str, terminal: &str, last_seen: i64, state: &str| {
        let mut r = fixture_rec(
            id,
            state,
            last_seen,
            (state == "closed").then_some(last_seen),
            (state == "closed").then_some("pty-exit"),
        );
        r.terminal_id = terminal.to_string();
        r
    };
    write_fixture(
        &path,
        vec![
            mk("old-a", "shared", 1_000, "open"),
            mk("newest", "shared", 3_000, "open"),
            mk("old-b", "shared", 2_000, "open"),
            mk("solo", "other-term", 5_000, "open"),
            mk("already-closed", "shared", 9_000, "closed"),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let closed = store.repair_terminal_id_collisions();
    assert_eq!(closed, 2, "the two older open rows on the shared terminal");

    // Newest open on the shared terminal survives.
    assert_eq!(store.get("newest").unwrap().state, "open");
    for dead in ["old-a", "old-b"] {
        let r = store.get(dead).unwrap();
        assert_eq!(r.state, "closed", "{dead} collapsed");
        assert_eq!(r.close_reason.as_deref(), Some("superseded-terminal-reuse"));
    }
    // Distinct-terminal row untouched.
    assert_eq!(
        store.get("solo").unwrap().state,
        "open",
        "distinct terminal untouched"
    );
    // A pre-closed row is not the "kept" one and is not re-closed/mutated.
    assert_eq!(
        store.get("already-closed").unwrap().close_reason.as_deref(),
        Some("pty-exit"),
        "an already-closed row is ignored (not counted, not re-stamped)"
    );

    // Idempotent: a second pass closes nothing.
    assert_eq!(store.repair_terminal_id_collisions(), 0, "idempotent");
}

/// The boot repair must be CONFIRMED-aware: a CONFIRMED real session on a
/// reused terminal must survive even when an UNCONFIRMED phantom (a later
/// zone-move / boot re-assert) carries a marginally NEWER `last_seen_at`.
/// A naive newest-only rank would close the real session — so it would never
/// restore — keeping a placeholder phantom instead. Mirrors `open_authority_key`.
#[test]
fn repair_terminal_id_collisions_keeps_confirmed_over_newer_unconfirmed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let mk = |id: &str, last_seen: i64, confirmed: Option<i64>| {
        let mut r = fixture_rec(id, "open", last_seen, None, None);
        r.terminal_id = "shared".to_string();
        r.confirmed_at = confirmed;
        r
    };
    write_fixture(
        &path,
        vec![
            mk("real-confirmed", 1_000, Some(1_000)),
            mk("phantom-newer", 2_000, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    assert_eq!(store.repair_terminal_id_collisions(), 1);
    assert_eq!(
        store.get("real-confirmed").unwrap().state,
        "open",
        "the confirmed real session survives despite an older last_seen"
    );
    let phantom = store.get("phantom-newer").unwrap();
    assert_eq!(
        phantom.state, "closed",
        "the unconfirmed newer phantom is closed"
    );
    assert_eq!(
        phantom.close_reason.as_deref(),
        Some("superseded-terminal-reuse")
    );
}

/// `rekey_session` moves a row from the old id to the new id, updating the
/// record's own `claude_session_id`, and is a safe no-op on the edge cases.
#[test]
fn rekey_session_moves_row_and_guards_edges() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    let mut r = rec("old");
    r.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
    store.record_open(r);

    store.rekey_session("old", "new");
    assert!(store.get("old").is_none(), "old key removed");
    let moved = store.get("new").expect("row present under new id");
    assert_eq!(moved.claude_session_id, "new", "record id updated to match");
    assert_eq!(moved.origin.as_deref(), Some(ORIGIN_AUTHORITATIVE));

    // Durable across a reload.
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.get("new").is_some());

    // Edge: absent old id — no-op, no panic.
    store.rekey_session("ghost", "whatever");
    assert!(store.get("whatever").is_none());

    // Edge: old == new — no-op.
    store.rekey_session("new", "new");
    assert!(store.get("new").is_some());

    // Edge: target already present — refuse to clobber.
    store.record_open(rec("other"));
    store.rekey_session("other", "new");
    assert!(store.get("other").is_some(), "source kept on refusal");
    assert_eq!(
        store.get("new").unwrap().claude_session_id,
        "new",
        "target untouched on refusal"
    );
}

/// `update_title_by_terminal` durably persists a rename keyed by
/// terminal id (Phase 3 item 4: the rename call sites hold only a
/// terminal id), survives a reload, and no-ops cleanly on the edges
/// (unknown terminal, closed record, already-current title).
#[test]
fn update_title_by_terminal_persists_rename_and_guards_edges() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-1")); // terminal "term-abc", title "Claude 1"

    store.update_title_by_terminal("term-abc", "634 agent_pusher fix");
    assert_eq!(
        store.get("sess-1").unwrap().title.as_deref(),
        Some("634 agent_pusher fix")
    );

    // Durable: a fresh open from the same path (simulated restart) sees
    // the renamed title, not the spawn-time one.
    let reloaded = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        reloaded.get("sess-1").unwrap().title.as_deref(),
        Some("634 agent_pusher fix"),
        "rename must survive restart"
    );

    // Edge: unknown terminal — no-op, no panic, nothing changed.
    store.update_title_by_terminal("no-such-terminal", "whatever");
    assert_eq!(
        store.get("sess-1").unwrap().title.as_deref(),
        Some("634 agent_pusher fix")
    );

    // Edge: closed record no longer matches (only OPEN rows are keyed by
    // terminal — a fresh PTY may reuse nothing, but a stale closed row
    // must not swallow the rename).
    store.record_close("sess-1", "pty-exit");
    store.update_title_by_terminal("term-abc", "should-not-land");
    assert_eq!(
        store.get("sess-1").unwrap().title.as_deref(),
        Some("634 agent_pusher fix"),
        "a closed record is not renamed"
    );
}

/// `remove_session` deletes a row outright (phantom-prune) so it never
/// resurrects on a future boot, and is a no-op when the id is absent.
#[test]
fn remove_session_deletes_row_and_is_noop_when_absent() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("phantom"));
    assert!(store.get("phantom").is_some());

    store.remove_session("phantom");
    assert!(store.get("phantom").is_none(), "row deleted");
    // Durable across reload — the row stays gone (not just closed-in-grace).
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.get("phantom").is_none(), "stays removed after reload");
    assert!(
        store.restorable_records(None, true).is_empty(),
        "a removed phantom is never restorable"
    );
    // Absent id — no panic, no write.
    store.remove_session("ghost");
}

#[test]
fn record_open_inserts_and_persists() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    {
        let store = SessionLifecycleStore::open(&path).unwrap();
        store.record_open(rec("sess-1"));
        let open = store.open_records();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].claude_session_id, "sess-1");
        assert_eq!(open[0].state, "open");
        assert!(open[0].opened_at > 0);
        assert_eq!(open[0].opened_at, open[0].last_seen_at);
    }
    // Survives a "restart".
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(store.open_records().len(), 1);
}

#[test]
fn record_open_dedups_by_key_and_preserves_opened_at() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("sess-1"));
    let opened_at = store.open_records()[0].opened_at;

    std::thread::sleep(std::time::Duration::from_millis(2));

    // Re-open the SAME claude session id with a new zone/terminal.
    let mut r2 = rec("sess-1");
    r2.zone_index = 9;
    r2.terminal_id = "term-xyz".to_string();
    r2.title = Some("Claude 1 (moved)".to_string());
    store.record_open(r2);

    let open = store.open_records();
    // Structural dedup: still exactly one record (no duplicate).
    assert_eq!(open.len(), 1, "same claude_session_id must not duplicate");
    assert_eq!(open[0].opened_at, opened_at, "opened_at preserved");
    assert!(open[0].last_seen_at > opened_at, "last_seen_at bumped");
    assert_eq!(open[0].zone_index, 9, "zone refreshed");
    assert_eq!(open[0].terminal_id, "term-xyz", "terminal refreshed");
}

#[test]
fn record_open_reopens_a_closed_record() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-1"));
    store.record_close("sess-1", "poll-dead");
    assert!(store.open_records().is_empty());

    store.record_open(rec("sess-1"));
    let open = store.open_records();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].state, "open");
    assert!(open[0].closed_at.is_none());
    assert!(open[0].close_reason.is_none());
}

#[test]
fn record_close_marks_closed_and_is_noop_when_absent_or_double() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // Absent → no-op, no panic.
    store.record_close("ghost", "x");

    store.record_open(rec("sess-1"));
    store.record_close("sess-1", "poll-dead");
    assert!(store.open_records().is_empty());

    // Double close → no-op.
    store.record_close("sess-1", "again");

    // Reload and confirm the closed record persisted with its reason.
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.open_records().is_empty());
}

/// The HEALTHY path: the pair the tab carries matches the pair
/// `record_open` bound, so the close lands on its own record and reports
/// `Closed`. A `Redirected` here would itself be the bug report.
#[test]
fn record_close_checked_closes_its_own_record_when_the_pair_matches() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(auth_on("mine", "term-mine"));
    store.record_open(auth_on("neighbour", "term-neighbour"));

    let outcome = store.record_close_checked("mine", Some("term-mine"), "explicit");

    assert_eq!(
        outcome,
        CloseOutcome::Closed {
            claude_session_id: "mine".to_string()
        }
    );
    assert_eq!(store.get("mine").unwrap().state, "closed");
    assert_eq!(
        store.get("mine").unwrap().close_reason.as_deref(),
        Some("explicit")
    );
    assert_eq!(
        store.get("neighbour").unwrap().state,
        "open",
        "a matched-pair close must not touch any other terminal's record"
    );
}

/// THE DEFECT. A tab carrying a STALE `claudeSessionId` — one that keys a
/// different terminal's record — must close the record its OWN terminal
/// owns, and must leave the stale target open. Under the old csid-only
/// contract this closed the foreign record and the live pty's record never
/// closed at all.
#[test]
fn record_close_checked_redirects_a_stale_csid_to_the_callers_own_terminal() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    // The record the stale id actually keys — a DIFFERENT terminal.
    store.record_open(auth_on("stale-foreign", "term-other"));
    // The record the caller's terminal really owns.
    store.record_open(auth_on("live-real", "term-live"));

    let outcome = store.record_close_checked("stale-foreign", Some("term-live"), "explicit");

    assert_eq!(
        outcome,
        CloseOutcome::Redirected {
            requested: "stale-foreign".to_string(),
            closed: "live-real".to_string(),
            terminal_id: "term-live".to_string(),
        }
    );
    assert_eq!(
        store.get("live-real").unwrap().state,
        "closed",
        "the caller's OWN terminal's record is the one that closes"
    );
    assert_eq!(
        store.get("live-real").unwrap().close_reason.as_deref(),
        Some("explicit")
    );
    assert_eq!(
        store.get("stale-foreign").unwrap().state,
        "open",
        "the foreign record the stale id named must NOT be closed"
    );
}

/// `latest_open_on_terminal` ranks hook order, then `confirmed_at`, then
/// `last_seen_at` / `opened_at`, then the id — so it is deterministic on a
/// full tie, never names the excluded id, and ignores closed rows and other
/// terminals.
#[test]
fn latest_open_on_terminal_ranks_deterministically() {
    for _ in 0..32 {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        // Inserted as `observed` + unconfirmed, which supersedes nothing,
        // then confirmed in place — how several open confirmed rows come to
        // share one terminal without `record_open`'s supersede arm.
        for id in ["b-tied", "a-tied", "c-tied"] {
            let mut r = rec(id);
            r.terminal_id = "term-t".to_string();
            r.origin = Some(ORIGIN_OBSERVED.to_string());
            store.record_open(r);
        }
        store.record_open(auth_on("other-term", "term-elsewhere"));
        store.note_session_start_hook("other-term");
        {
            let mut m = store.map.lock().unwrap();
            for id in ["a-tied", "b-tied", "c-tied"] {
                let r = m.get_mut(id).unwrap();
                r.confirmed_at = Some(5);
                r.last_seen_at = 7;
                r.opened_at = 7;
            }
        }
        // Full tie: the greatest id, every time.
        assert_eq!(
            store
                .latest_open_on_terminal("term-t", "nobody")
                .unwrap()
                .claude_session_id,
            "c-tied"
        );
        // The excluded id is never the answer.
        assert_eq!(
            store
                .latest_open_on_terminal("term-t", "c-tied")
                .unwrap()
                .claude_session_id,
            "b-tied"
        );
        // A hook outranks every timestamp, and the latest hook wins.
        store.note_session_start_hook("b-tied");
        store.note_session_start_hook("a-tied");
        assert_eq!(
            store
                .latest_open_on_terminal("term-t", "nobody")
                .unwrap()
                .claude_session_id,
            "a-tied"
        );
        // A closed row is not a candidate however recent its hook.
        store.record_close("a-tied", "explicit");
        assert_eq!(
            store
                .latest_open_on_terminal("term-t", "nobody")
                .unwrap()
                .claude_session_id,
            "b-tied"
        );
        assert!(store.latest_open_on_terminal("term-none", "x").is_none());
        assert!(store.latest_open_on_terminal("", "x").is_none());
    }
}

/// The redirect must be correct — and DETERMINISTIC — when the caller's
/// terminal carries more than one open row (the per-terminal single-open-row
/// invariant is enforced at open and at boot, never in between). It ranks
/// with `open_authority_key`, so a CONFIRMED row wins over a newer
/// unconfirmed one, and every candidate is named.
///
/// A first-match `.find()` over the `HashMap` would
/// close a nondeterministic row here — a worse failure than the silent
/// no-op it replaces, because it is not reproducible.
#[test]
fn record_close_checked_multi_match_closes_the_authoritative_row_deterministically() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();

    // Two open rows on ONE terminal. Built without `record_open`'s
    // supersede arm (an `observed` UNCONFIRMED bind deliberately does not
    // supersede — see `record_open_observed_unconfirmed_does_not_supersede`),
    // which is how this population arises in production.
    let mut confirmed = auth_on("real-confirmed", "term-dup");
    confirmed.confirmed_at = Some(1_000);
    confirmed.last_seen_at = 1_000;
    store.record_open(confirmed);

    let mut newer_unconfirmed = rec("phantom-newer");
    newer_unconfirmed.terminal_id = "term-dup".to_string();
    newer_unconfirmed.origin = Some(ORIGIN_OBSERVED.to_string());
    store.record_open(newer_unconfirmed);

    assert_eq!(
        store
            .open_records()
            .iter()
            .filter(|r| r.terminal_id == "term-dup")
            .count(),
        2,
        "fixture precondition: the terminal really carries two open rows"
    );

    // Run it repeatedly on identical fixtures: the SAME row must win each
    // time, regardless of HashMap iteration order.
    for _ in 0..8 {
        let dir = tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let mut confirmed = auth_on("real-confirmed", "term-dup");
        confirmed.confirmed_at = Some(1_000);
        confirmed.last_seen_at = 1_000;
        store.record_open(confirmed);
        let mut newer = rec("phantom-newer");
        newer.terminal_id = "term-dup".to_string();
        newer.origin = Some(ORIGIN_OBSERVED.to_string());
        store.record_open(newer);

        let outcome = store.record_close_checked("no-such-session", Some("term-dup"), "explicit");

        match outcome {
            CloseOutcome::RedirectedAmbiguous {
                ref requested,
                ref closed,
                ref terminal_id,
                ref candidates,
            } => {
                assert_eq!(requested, "no-such-session");
                assert_eq!(
                    closed, "real-confirmed",
                    "the CONFIRMED row outranks a newer unconfirmed one"
                );
                assert_eq!(terminal_id, "term-dup");
                assert_eq!(
                    candidates,
                    &vec!["phantom-newer".to_string(), "real-confirmed".to_string()],
                    "every colliding row is named, in a deterministic order"
                );
            }
            other => panic!("expected RedirectedAmbiguous, got {other:?}"),
        }
        assert_eq!(store.get("real-confirmed").unwrap().state, "closed");
        assert_eq!(
            store.get("phantom-newer").unwrap().state,
            "open",
            "only the ranked winner closes"
        );
    }
}

/// A close that resolves to nothing mutates nothing, fires no observer, and
/// is reported as `NotFound` — never as a success.
#[test]
fn record_close_checked_not_found_mutates_nothing_and_fires_no_observer() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    let fired = std::sync::Arc::new(AtomicUsize::new(0));
    {
        let fired = fired.clone();
        store.attach_close_observer(move |_| {
            fired.fetch_add(1, Ordering::SeqCst);
        });
    }
    store.record_open(auth_on("bystander", "term-bystander"));

    let outcome = store.record_close_checked("unknown-sess", Some("term-gone"), "explicit");

    assert_eq!(
        outcome,
        CloseOutcome::NotFound {
            requested: "unknown-sess".to_string(),
            terminal_id: Some("term-gone".to_string()),
        }
    );
    assert_eq!(
        store.get("bystander").unwrap().state,
        "open",
        "an unresolvable close must not fall back onto some other row"
    );
    assert_eq!(fired.load(Ordering::SeqCst), 0, "no observer on NotFound");
}

/// The account-migration hand-off: the respawn re-keys the session's row
/// onto the NEW terminal while the old PTY is still alive, then the old
/// PTY is closed. Its exit-close names `(session, old terminal)`, which no
/// open row owns — without the register that is a mis-bound `NotFound`
/// (a `warn!`) on every migration. With it: `Superseded`, nothing mutated,
/// no observer (coord must not see a `Closed` for a live session), and
/// the entry is consumed so a genuinely stale close of the same id later
/// still surfaces as `NotFound`.
#[test]
fn record_close_checked_answers_a_superseded_terminal_without_touching_the_live_row() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    let fired = std::sync::Arc::new(AtomicUsize::new(0));
    {
        let fired = fired.clone();
        store.attach_close_observer(move |_| {
            fired.fetch_add(1, Ordering::SeqCst);
        });
    }
    // The session opens on the old terminal, then the respawn re-keys the
    // SAME row onto the new one (what `record_pinned_session_open` does).
    store.record_open(auth_on("moving", "term-old"));
    store.record_open(auth_on("moving", "term-new"));
    assert_eq!(store.get("moving").unwrap().terminal_id, "term-new");

    store.mark_terminal_superseded("term-old", "term-new");
    let outcome = store.record_close_checked("moving", Some("term-old"), "pty-exit");

    assert_eq!(
        outcome,
        CloseOutcome::TerminalSuperseded {
            requested: "moving".to_string(),
            terminal_id: "term-old".to_string(),
        }
    );
    let live = store.get("moving").unwrap();
    assert_eq!(live.state, "open", "the live row must not be closed");
    assert_eq!(live.terminal_id, "term-new");
    assert!(live.close_reason.is_none());
    assert_eq!(
        fired.load(Ordering::SeqCst),
        0,
        "no coord Closed for a live session"
    );

    // Consumed: the same close again is the ordinary mis-bound answer.
    assert_eq!(
        store.record_close_checked("moving", Some("term-old"), "pty-exit"),
        CloseOutcome::NotFound {
            requested: "moving".to_string(),
            terminal_id: Some("term-old".to_string()),
        }
    );
    // And the new terminal's own close still works as a real close.
    assert_eq!(
        store.record_close_checked("moving", Some("term-new"), "explicit"),
        CloseOutcome::Closed {
            claude_session_id: "moving".to_string()
        }
    );
    assert_eq!(fired.load(Ordering::SeqCst), 1);
}

/// The register is keyed by terminal id alone: a superseded entry must not
/// intercept a close that names a DIFFERENT terminal; a blank id is never
/// registered; and `unmark` withdraws an entry that will never be consumed
/// (the old PTY was already gone when the migration went to close it).
#[test]
fn superseded_register_matches_only_the_named_terminal() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(auth_on("a", "term-a"));
    store.mark_terminal_superseded("term-elsewhere", "term-x");
    store.mark_terminal_superseded("   ", "term-x");
    assert_eq!(
        store.superseded_terminal_count(),
        1,
        "a blank id is never registered"
    );

    assert_eq!(
        store.record_close_checked("a", Some("term-a"), "explicit"),
        CloseOutcome::Closed {
            claude_session_id: "a".to_string()
        }
    );
    assert_eq!(
        store.superseded_terminal_count(),
        1,
        "a different terminal's close consumes nothing"
    );

    store.unmark_terminal_superseded("term-elsewhere");
    store.unmark_terminal_superseded("never-marked");
    assert_eq!(store.superseded_terminal_count(), 0);
}

/// `expect_terminal_id == None` is byte-identical to the historical
/// csid-only close on the same fixture — the internal closers
/// (`poll-dead`, `never-started`, `no-terminal`) keep their
/// semantics exactly.
#[test]
fn record_close_checked_without_a_terminal_matches_legacy_record_close() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(auth_on("sess", "term-x"));

    // Closes despite naming no terminal, and despite the row living on a
    // terminal the caller never mentions.
    assert_eq!(
        store.record_close_checked("sess", None, "poll-dead"),
        CloseOutcome::Closed {
            claude_session_id: "sess".to_string()
        }
    );
    assert_eq!(store.get("sess").unwrap().state, "closed");
    assert_eq!(
        store.get("sess").unwrap().close_reason.as_deref(),
        Some("poll-dead")
    );
    // Repeat / absent closes stay no-ops.
    assert_eq!(
        store.record_close_checked("sess", None, "again"),
        CloseOutcome::AlreadyClosed {
            claude_session_id: "sess".to_string()
        }
    );
    assert_eq!(
        store.get("sess").unwrap().close_reason.as_deref(),
        Some("poll-dead"),
        "a repeat close does not re-stamp the reason"
    );
    assert_eq!(
        store.record_close_checked("ghost", None, "x"),
        CloseOutcome::NotFound {
            requested: "ghost".to_string(),
            terminal_id: None,
        }
    );
}

/// The provider-less-shell carve-out: a plain shell's provisional row is
/// retired `never-started` WHILE ITS TERMINAL IS ALIVE, by design. The
/// user then closes that tab. That must read as a benign repeat close of
/// this terminal's own record — not as a resolution failure, and above all
/// not as a redirect onto some other terminal's live session.
#[test]
fn record_close_checked_repeat_close_of_own_retired_row_is_already_closed() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(auth_on("bare-shell", "term-shell"));
    store.record_open(auth_on("someone-else", "term-else"));
    store.record_close("bare-shell", "never-started");

    let outcome = store.record_close_checked("bare-shell", Some("term-shell"), "explicit");

    assert_eq!(
        outcome,
        CloseOutcome::AlreadyClosed {
            claude_session_id: "bare-shell".to_string()
        }
    );
    assert_eq!(
        store.get("bare-shell").unwrap().close_reason.as_deref(),
        Some("never-started"),
        "the by-design retirement reason is not overwritten"
    );
    assert_eq!(
        store.get("someone-else").unwrap().state,
        "open",
        "a zero-open-row terminal must not redirect onto a neighbour"
    );
}

/// Under a redirect the close observer must fire for the record ACTUALLY
/// closed, never the one requested — otherwise coord's row for the live
/// session is evicted while the dead one leaks.
#[test]
fn record_close_checked_redirect_fires_observer_with_the_redirected_to_id() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    {
        let seen = seen.clone();
        store.attach_close_observer(move |csid| seen.lock().unwrap().push(csid.to_string()));
    }
    store.record_open(auth_on("stale-foreign", "term-other"));
    store.record_open(auth_on("live-real", "term-live"));

    store.record_close_checked("stale-foreign", Some("term-live"), "pty-exit");

    assert_eq!(
        *seen.lock().unwrap(),
        vec!["live-real".to_string()],
        "the observer sees the id that closed, not the id that was asked for"
    );
}

#[test]
fn close_observer_fires_only_on_real_open_to_closed_transitions() {
    let _amb = crate::test_env::isolated_ambient();
    // Fabric Phase 3 (review W2): the observer must see every REAL
    // open→closed flip exactly once — never an absent-id close, never a
    // repeat close. Unattached stores (every other test) stay no-op.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();

    let fired = std::sync::Arc::new(AtomicUsize::new(0));
    let last: std::sync::Arc<std::sync::Mutex<String>> = Default::default();
    {
        let fired = fired.clone();
        let last = last.clone();
        store.attach_close_observer(move |csid| {
            fired.fetch_add(1, Ordering::SeqCst);
            *last.lock().unwrap() = csid.to_string();
        });
    }

    // Absent id → no fire.
    store.record_close("ghost", "x");
    assert_eq!(fired.load(Ordering::SeqCst), 0);

    // Real transition → fires once with the csid.
    store.record_open(rec("sess-1"));
    store.record_close("sess-1", "poll-dead");
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    assert_eq!(last.lock().unwrap().as_str(), "sess-1");

    // Repeat close → no refire.
    store.record_close("sess-1", "again");
    assert_eq!(fired.load(Ordering::SeqCst), 1);

    // Reopen + close again → a second REAL transition fires again.
    store.record_open(rec("sess-1"));
    store.record_close("sess-1", "poll-dead");
    assert_eq!(fired.load(Ordering::SeqCst), 2);
}

#[test]
fn restorable_records_includes_open_and_in_grace_pty_exit_only() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // An open record (hard-crash case) — IS restorable.
    store.record_open(rec("open-sess"));
    // A pty-exit close (graceful-restart case) — IS restorable while in grace.
    store.record_open(rec("pty-exit-sess"));
    store.record_close("pty-exit-sess", "pty-exit");
    // An explicit/user close — is NOT restorable.
    store.record_open(rec("user-closed-sess"));
    store.record_close("user-closed-sess", "explicit");
    // A pty-exit close aged beyond the grace window — is NOT restorable.
    store.record_open(rec("stale-pty-exit-sess"));
    store.record_close("stale-pty-exit-sess", "pty-exit");

    let now = Utc::now().timestamp_millis();

    // Age the stale pty-exit record's closed_at past the grace window.
    age_persisted_records(&store, &path, |m| {
        m.get_mut("stale-pty-exit-sess").unwrap().closed_at =
            Some(now - RESTORABLE_PTY_EXIT_MS - 1000);
    });
    let store = SessionLifecycleStore::open(&path).unwrap();

    let mut ids: Vec<String> = store
        .restorable_records(None, true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["open-sess".to_string(), "pty-exit-sess".to_string()],
        "open + in-grace pty-exit restorable; explicit + stale-pty-exit excluded"
    );

    // open_records() keeps strict-open semantics (only the open one).
    let open_ids: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    assert_eq!(open_ids, vec!["open-sess".to_string()]);
}

/// THE headline behaviour of plan
/// `2026-09-01-session-finished-marker-and-unfinished-resume`: a rebuilt
/// runner brings back only the UNFINISHED sessions.
///
/// Finished outranks every liveness arm — an `open` row that would otherwise
/// be admitted unconditionally is excluded once marked.
#[test]
fn restorable_records_excludes_finished_sessions() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("unfinished-open"));
    store.record_open(rec("finished-open"));
    store.record_open(rec("finished-pty-exit"));
    store.record_close("finished-pty-exit", "pty-exit");

    let _ = store.set_finished("finished-open", true, Some("work landed".into()));
    let _ = store.set_finished("finished-pty-exit", true, None);

    let ids: Vec<String> = store
        .restorable_records(None, true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();

    assert_eq!(
        ids,
        vec!["unfinished-open".to_string()],
        "a finished session is never restorable — not while `open`, and not \
         within the pty-exit grace either"
    );
}

/// `finished` is the WORK axis and `state` is LIVENESS. Marking must not
/// close the session, and closing must not finish it — collapsing the two is
/// the mistake the field exists to avoid.
#[test]
fn finishing_a_session_never_touches_its_liveness_state() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("live"));
    let after = store
        .set_finished("live", true, Some("done".into()))
        .expect("marking a known session reports the new record")
        .record;

    assert_eq!(
        after.state, "open",
        "marking finished must NOT close the PTY"
    );
    assert!(after.closed_at.is_none());
    assert!(after.finished_at.is_some());
    assert_eq!(after.finish_reason.as_deref(), Some("done"));
    assert!(
        !after.finish_synced,
        "a fresh mark is unsynced — coord is still owed the write"
    );

    // And the converse: closing a session says nothing about its work axis.
    store.record_open(rec("other"));
    store.record_close("other", "pty-exit");
    let closed = store
        .all_records()
        .into_iter()
        .find(|r| r.claude_session_id == "other")
        .unwrap();
    assert_eq!(closed.state, "closed");
    assert!(
        closed.finished_at.is_none(),
        "a crashed/exited session is CLOSED, not FINISHED"
    );
}

/// A second finish that carries a reason is a new declaration: it
/// RESTAMPS the mark (always a DIFFERENT stamp), clears `finish_synced` and re-sends
/// the finish carrying the NEW stamp — so a revived session finished again
/// is dated by its second finish. A reason-less re-finish is the
/// read-back / retry door and moves nothing. Unmark must clear the marker,
/// the reason and the sync flag together.
#[test]
fn a_second_finish_restamps_and_re_sends_and_unmark_leaves_no_residue() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    // The finished_at of every record the observer was handed — i.e. of
    // every coord write that was (re-)sent.
    let sent: std::sync::Arc<std::sync::Mutex<Vec<Option<i64>>>> = Default::default();
    {
        let sent = sent.clone();
        store.attach_finish_observer(move |rec| {
            sent.lock().unwrap().push(rec.finished_at);
            FinishSync::LocalOnly(LocalOnlyReason::NoCoordSession)
        });
    }

    store.record_open(rec("s"));
    let first = store.set_finished("s", true, Some("r1".into())).unwrap();
    assert_eq!(first.changed, FinishChange::Marker);
    let stamped = first.record.finished_at.unwrap();
    assert_eq!(*sent.lock().unwrap(), vec![Some(stamped)]);

    // The read-back: reason-less, so it reads and moves nothing.
    let read = store.set_finished("s", true, None).unwrap();
    assert_eq!(
        read.changed,
        FinishChange::None,
        "a reason-less re-finish is a read — no write, no restamp"
    );
    assert_eq!(read.record.finished_at, Some(stamped));
    assert_eq!(
        sent.lock().unwrap().len(),
        2,
        "on an UNSYNCED mark the read re-offers the owed write (the retry door)"
    );
    assert_eq!(
        sent.lock().unwrap()[1],
        Some(stamped),
        "with the SAME stamp"
    );

    store.mark_finish_synced("s", Some(stamped));
    assert!(store.get("s").unwrap().finish_synced);
    assert_eq!(
        store.set_finished("s", true, None).unwrap().coord,
        FinishSync::LocalOnly(LocalOnlyReason::NotRequeued),
        "a reason-less read on a SYNCED mark owes coord nothing"
    );
    assert_eq!(
        store.get("s").unwrap().finished_at,
        Some(stamped),
        "and the read-back still sees the synced mark it is reading"
    );
    assert!(store.get("s").unwrap().finish_synced);
    assert_eq!(sent.lock().unwrap().len(), 2);

    // The session is revived and finishes AGAIN: a declaration, with a
    // reason. Even the SAME reason restamps.
    let again = store.set_finished("s", true, Some("r1".into())).unwrap();
    assert_eq!(
        again.changed,
        FinishChange::Marker,
        "a second declared finish rewrites the mark itself"
    );
    let restamped = again.record.finished_at.unwrap();
    assert_ne!(
        restamped, stamped,
        "the second finish is dated by itself, never by the first"
    );
    assert!(
        !again.record.finish_synced,
        "a restamped mark is unsynced — coord is owed the new time"
    );
    assert_eq!(
        again.coord,
        FinishSync::LocalOnly(LocalOnlyReason::NoCoordSession),
        "the restamp was handed to the forwarder, not reported not_requeued"
    );
    assert_eq!(
        sent.lock().unwrap().last(),
        Some(&Some(restamped)),
        "the re-sent write carries the NEW stamp"
    );
    let on_disk = SessionLifecycleStore::open(&path)
        .unwrap()
        .get("s")
        .expect("persisted");
    assert_eq!(
        on_disk.finished_at,
        Some(restamped),
        "the restamp is durable"
    );
    assert!(!on_disk.finish_synced);

    let un = store.set_finished("s", false, None).unwrap();
    assert_eq!(un.changed, FinishChange::Marker);
    let un = un.record;
    assert!(un.finished_at.is_none());
    assert!(un.finish_reason.is_none(), "the reason is cleared with it");
    assert!(
        !un.finish_synced,
        "the sync flag is cleared too — an unmarked session owes coord \
         nothing about a marker it no longer has"
    );

    assert!(
        matches!(
            store.set_finished("nope", true, None),
            Err(SetFinishedError::UnknownSession)
        ),
        "an unknown session is UnknownSession, never a panic or a phantom record"
    );
}

/// Review round 2, L-a: every observer hand-off — a fresh mark, the
/// no-op retry's snapshot, an unmark — runs with `finish_forward` held,
/// so a concurrent unmark cannot enqueue between a snapshot and its
/// forward and leave coord `finished` for an unmarked record.
#[test]
fn set_finished_holds_the_forward_serializer_across_the_observer() {
    let dir = tempdir().unwrap();
    let store =
        std::sync::Arc::new(SessionLifecycleStore::open(dir.path().join("s.json")).unwrap());
    let held: std::sync::Arc<std::sync::Mutex<Vec<bool>>> = Default::default();
    {
        let weak = std::sync::Arc::downgrade(&store);
        let held = held.clone();
        store.attach_finish_observer(move |_rec| {
            let store = weak.upgrade().expect("store alive");
            held.lock()
                .unwrap()
                .push(store.finish_forward.try_lock().is_err());
            FinishSync::LocalOnly(LocalOnlyReason::NoCoordSession)
        });
    }
    store.record_open(rec("s"));
    let _ = store.set_finished("s", true, None); // fresh mark
    let _ = store.set_finished("s", true, None); // no-op retry (unsynced)
    let _ = store.set_finished("s", false, None); // unmark
    assert_eq!(
        *held.lock().unwrap(),
        vec![true, true, true],
        "the serializer is held for every forward"
    );
    assert!(
        store.finish_forward.try_lock().is_ok(),
        "and released when set_finished returns"
    );
}

/// Review (adoption) M2: the late-delivery door reads AND runs its
/// enqueue with `finish_forward` held, so it serializes against an unmark
/// exactly as `set_finished`'s own forwards do.
#[test]
fn late_delivery_door_holds_the_forward_serializer() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(rec("s"));
    let _ = store.set_finished("s", true, None);
    let (ids, held) = store.with_unsynced_finished_records(|recs| {
        (
            recs.into_iter()
                .map(|r| r.claude_session_id)
                .collect::<Vec<_>>(),
            store.finish_forward.try_lock().is_err(),
        )
    });
    assert_eq!(ids, vec!["s".to_string()], "the unsynced mark is offered");
    assert!(held, "the serializer is held while the delivery runs");
    assert!(
        store.finish_forward.try_lock().is_ok(),
        "and released when the door returns"
    );
}

/// A restamp only has to DIFFER from the old stamp (that is all the ACK
/// matcher needs): two finishes inside one millisecond still differ, and a
/// clock stepped backwards dates the new finish at `now` rather than
/// pushing it past the old one into the future.
#[test]
fn a_restamp_always_differs_and_never_runs_ahead_of_the_clock() {
    assert_eq!(restamp_at(1_000, 1_000), 1_001, "equal-now still differs");
    assert_eq!(
        restamp_at(1_000, 5_000),
        5_000,
        "a later clock is taken as is"
    );
    assert_eq!(
        restamp_at(1_000, 400),
        400,
        "a clock stepped back is not pushed into the future"
    );
    for (prev, now) in [(1_000, 1_000), (1_000, 5_000), (1_000, 400)] {
        assert_ne!(restamp_at(prev, now), prev);
    }
}

/// Review (adoption) L1, as amended by the restamp: a blank reason on an
/// already-finished mark is no reason — a read that rewrites nothing
/// (`changed: none`). Any real reason, the stored one included, is a new
/// declaration that restamps the mark (`changed: marker`).
#[test]
fn a_blank_reason_rewrites_nothing_and_any_reason_restamps() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(rec("s"));
    let stamped = store
        .set_finished("s", true, Some("done".into()))
        .expect("fresh mark")
        .record
        .finished_at;
    for r in ["", "   "] {
        let out = store
            .set_finished("s", true, Some(r.to_string()))
            .expect("known id");
        assert_eq!(
            out.changed,
            FinishChange::None,
            "reason {r:?} rewrote nothing"
        );
        assert_eq!(out.record.finish_reason.as_deref(), Some("done"));
        assert_eq!(out.record.finished_at, stamped, "and moved nothing");
    }
    let mut last = stamped;
    for r in ["done", "other"] {
        let out = store
            .set_finished("s", true, Some(r.to_string()))
            .expect("known id");
        assert_eq!(out.changed, FinishChange::Marker, "reason {r:?} restamps");
        assert_eq!(out.record.finish_reason.as_deref(), Some(r));
        assert_ne!(out.record.finished_at, last, "reason {r:?} moved the stamp");
        last = out.record.finished_at;
    }
}

/// The finish observer is the producer seam for coord's `Finished` outbox
/// row: it must fire for a fresh mark and re-fire for a no-op repeat while
/// the mark is UNSYNCED (the retry door), stay silent on a no-op unmark
/// and on an unknown id, and — once coord has ACKed — stay silent on a
/// reason-less re-finish but fire for a reason-carrying one (a restamp).
#[test]
fn finish_observer_fires_only_while_a_coord_write_is_owed() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    // (csid, finished?) per notification.
    let seen: std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>> = Default::default();
    {
        let seen = seen.clone();
        store.attach_finish_observer(move |rec| {
            seen.lock()
                .unwrap()
                .push((rec.claude_session_id.clone(), rec.finished_at.is_some()));
            FinishSync::LocalOnly(LocalOnlyReason::NoCoordSession)
        });
    }
    store.record_open(rec("s"));

    let _ = store.set_finished("ghost", true, None);
    let _ = store.set_finished("s", false, None);
    assert!(
        seen.lock().unwrap().is_empty(),
        "an unknown id and a no-op unmark never fire"
    );

    let _ = store.set_finished("s", true, None);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("s".to_string(), true)],
        "fresh mark fires"
    );

    let _ = store.set_finished("s", true, None);
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "a no-op repeat on an UNSYNCED mark re-fires — the retry door"
    );

    let _ = store.set_finished("s", true, Some("reason".into()));
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "a reason-carrying re-finish on an UNSYNCED mark restamps and re-fires"
    );

    store.mark_finish_synced("s", None);
    let _ = store.set_finished("s", true, None);
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "once coord has ACKed, a reason-less no-op owes coord nothing"
    );
    let _ = store.set_finished("s", true, Some("later reason".into()));
    assert_eq!(
        *seen.lock().unwrap().last().unwrap(),
        ("s".to_string(), true),
        "but a reason-carrying re-finish on a SYNCED mark restamps, so coord \
         is owed — and sent — the new finish"
    );
    assert_eq!(seen.lock().unwrap().len(), 4);

    let _ = store.set_finished("s", false, None);
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&("s".to_string(), false)),
        "a real unmark fires, so coord is told the session is NOT finished"
    );
}

/// After mark → unmark → re-mark, the first mark's late ACK must not stamp
/// the second mark synced before the second write has drained.
#[test]
fn a_stale_ack_never_syncs_a_newer_mark() {
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
    store.record_open(rec("s"));

    let first = store
        .set_finished("s", true, None)
        .unwrap()
        .record
        .finished_at;
    let _ = store.set_finished("s", false, None);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = store
        .set_finished("s", true, None)
        .unwrap()
        .record
        .finished_at;
    assert_ne!(first, second);

    store.mark_finish_synced("s", first);
    assert!(!store.get("s").unwrap().finish_synced, "stale ACK ignored");

    store.mark_finish_synced("s", second);
    assert!(
        store.get("s").unwrap().finish_synced,
        "the matching ACK stamps"
    );
}

/// An ACK that arrives after the operator unmarked the session must not
/// resurrect the marker.
#[test]
fn a_late_sync_ack_never_resurrects_a_cleared_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("s"));
    let _ = store.set_finished("s", true, None);
    let _ = store.set_finished("s", false, None);

    store.mark_finish_synced("s", None); // the in-flight ACK lands late

    let snap = store
        .all_records()
        .into_iter()
        .find(|r| r.claude_session_id == "s")
        .unwrap();
    assert!(snap.finished_at.is_none(), "still unmarked");
    assert!(!snap.finish_synced, "and still unsynced");
}

/// Every record already on disk predates these three fields. Without
/// `#[serde(default)]` the WHOLE registry fails to deserialize on the first
/// boot after the upgrade — losing exactly the sessions this feature exists
/// to preserve.
#[test]
fn legacy_records_without_the_finish_fields_still_deserialize() {
    let legacy = r#"{
            "claudeSessionId": "legacy-1",
            "pageId": "default",
            "zoneIndex": 0,
            "terminalId": "term-1",
            "openedAt": 1,
            "lastSeenAt": 2,
            "state": "open"
        }"#;
    let rec: TerminalSessionRecord =
        serde_json::from_str(legacy).expect("a pre-finish record must still load");
    assert_eq!(rec.claude_session_id, "legacy-1");
    assert!(rec.finished_at.is_none());
    assert!(rec.finish_reason.is_none());
    assert!(!rec.finish_synced);
}

/// B5 (phantom-id plan): with a transcript probe attached, every history
/// entry carries the write-time restorability verdict — so a recovery line
/// naming a phantom id says so, instead of looking identical to a healthy
/// one (the 2026-07-08 failure). Without a probe the fields stay absent.
#[test]
fn snapshot_history_stamps_restorability_when_a_probe_is_attached() {
    #[derive(Debug)]
    struct FakeProbe;
    impl TranscriptProbe for FakeProbe {
        fn transcript_exists(&self, session_id: &str, _wd: Option<&str>) -> bool {
            session_id == "sess-1"
        }
    }

    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    let history_path = dir.path().join("session-snapshots.jsonl");
    store.attach_snapshot_history(Arc::new(
        crate::session::snapshot_history::SnapshotHistory::open(&history_path).unwrap(),
    ));
    store.attach_transcript_probe(Arc::new(FakeProbe));

    let last_line = |path: &Path| -> serde_json::Value {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        let line = contents.lines().filter(|l| !l.trim().is_empty()).last();
        serde_json::from_str(line.expect("a snapshot line")).unwrap()
    };

    // A transcript-backed, CONFIRMED session → restorable:true.
    let mut healthy = rec("sess-1");
    healthy.confirmed_at = Some(42);
    store.record_open(healthy);
    let s = &last_line(&history_path)["sessions"][0];
    assert_eq!(s["claudeSessionId"], "sess-1");
    assert_eq!(s["transcriptExists"], true);
    assert_eq!(
        s["restorable"], true,
        "the newest entry carries restorable:true + a real id"
    );

    // A phantom (no transcript, unconfirmed) → the line names it as such.
    let mut phantom = rec("0b32d739");
    phantom.terminal_id = "943aa0b6".to_string();
    phantom.confirmed_at = None;
    store.record_open(phantom);
    let sessions = last_line(&history_path)["sessions"]
        .as_array()
        .unwrap()
        .clone();
    let p = sessions
        .iter()
        .find(|s| s["claudeSessionId"] == "0b32d739")
        .expect("the phantom is in the snapshot");
    assert_eq!(p["transcriptExists"], false);
    assert_eq!(p["restorable"], false, "the phantom is flagged, not silent");
}

/// Phase 4 (session-snapshot history): layout-meaningful mutations
/// append CHANGE snapshots to the attached history; liveness churn
/// (`touch`, restore-pending markers) and `prune` do NOT — and pruning
/// the registry never removes already-appended history lines (the
/// history outlives the registry's retention).
#[test]
fn snapshot_history_captures_changes_but_not_liveness_churn_or_prune() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    let history_path = dir.path().join("session-snapshots.jsonl");
    store.attach_snapshot_history(Arc::new(
        crate::session::snapshot_history::SnapshotHistory::open(&history_path).unwrap(),
    ));

    let lines = |path: &Path| -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };

    // Open → one change snapshot carrying the full recovery tuple.
    store.record_open(rec("sess-1"));
    let recs = lines(&history_path);
    assert_eq!(recs.len(), 1, "record_open appends a change snapshot");
    assert_eq!(recs[0]["reason"], "change");
    let s = &recs[0]["sessions"][0];
    assert_eq!(s["claudeSessionId"], "sess-1");
    assert_eq!(s["configDir"], "C:/cfg");
    assert_eq!(s["workingDir"], "C:/repo");
    assert_eq!(s["provider"], "claude");
    assert_eq!(s["pageId"], "default");
    assert_eq!(s["zoneIndex"], 2);
    assert_eq!(s["title"], "Claude 1");
    assert_eq!(s["state"], "open");

    // Liveness churn must stay silent: touch, restore-pending markers,
    // an identical re-record (boot re-assert), and a snapshot-gated
    // heartbeat right after a change.
    store.touch("sess-1");
    store.mark_restore_pending("sess-1", true);
    store.clear_restore_pending("sess-1");
    store.record_open(rec("sess-1"));
    store.snapshot_heartbeat();
    assert_eq!(
        recs.len(),
        lines(&history_path).len(),
        "churn appends nothing"
    );

    // Close → a change snapshot with the alive-state fields.
    store.record_close("sess-1", "poll-dead");
    let recs = lines(&history_path);
    assert_eq!(recs.len(), 2, "record_close appends a change snapshot");
    let s = &recs[1]["sessions"][0];
    assert_eq!(s["state"], "closed");
    assert_eq!(s["closeReason"], "poll-dead");

    // Prune destroys the registry row (far-future now) but the history
    // keeps every appended line: the audit outlives `prune`.
    store.prune(
        Utc::now().timestamp_millis() + CLOSED_RETENTION_MS + 1_000,
        &HashSet::new(),
    );
    assert!(store.get("sess-1").is_none(), "registry row pruned");
    assert_eq!(lines(&history_path).len(), 2, "history retained past prune");
}

#[test]
fn touch_bumps_last_seen_only_when_present() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-1"));
    let before = store.open_records()[0].last_seen_at;
    std::thread::sleep(std::time::Duration::from_millis(2));
    store.touch("sess-1");
    let after = store.open_records()[0].last_seen_at;
    assert!(after > before);
    // Touching an absent id is a no-op (no panic).
    store.touch("ghost");
}

#[test]
fn prune_drops_old_closed_and_stale_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("fresh-open"));
    store.record_open(rec("stale-open"));
    store.record_open(rec("recent-closed"));
    store.record_open(rec("old-closed"));
    store.record_close("recent-closed", "done");
    store.record_close("old-closed", "done");

    let now = Utc::now().timestamp_millis();

    // No public mutator can push `last_seen_at`/`closed_at` into the past,
    // so age `stale-open` and `old-closed` past their thresholds on disk
    // and let the reopen below load them.
    age_persisted_records(&store, &path, |m| {
        m.get_mut("stale-open").unwrap().last_seen_at = now - OPEN_STALE_MS - 1000;
        m.get_mut("old-closed").unwrap().closed_at = Some(now - CLOSED_RETENTION_MS - 1000);
    });

    // Reopen so the store loads the aged timestamps, then prune. No
    // terminal is live, so retention alone decides.
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.prune(now, &HashSet::new());

    let store = SessionLifecycleStore::open(&path).unwrap();
    let raw = std::fs::read(&path).unwrap();
    let m: HashMap<String, TerminalSessionRecord> = serde_json::from_slice(&raw).unwrap();
    assert!(m.contains_key("fresh-open"), "fresh open survives");
    assert!(m.contains_key("recent-closed"), "recent closed survives");
    assert!(!m.contains_key("stale-open"), "stale open pruned");
    assert!(!m.contains_key("old-closed"), "old closed pruned");
    // open_records reflects the pruned set.
    let open_ids: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    assert_eq!(open_ids, vec!["fresh-open".to_string()]);
}

/// P4: prune retention vs terminal liveness. A closed row whose terminal
/// is STILL LIVE survives the prune past `CLOSED_RETENTION_MS` (deleting
/// it would make a recorded-then-closed terminal look never-recorded);
/// the same-aged closed row whose terminal is gone is pruned. The
/// open-stale (7d not-seen) path is gated identically.
#[test]
fn prune_keeps_rows_whose_terminal_is_still_live() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    let mut closed_live = rec("closed-live-term");
    closed_live.terminal_id = "term-live".to_string();
    let mut closed_live_older = rec("closed-live-term-older");
    closed_live_older.terminal_id = "term-live".to_string();
    let mut closed_gone = rec("closed-gone-term");
    closed_gone.terminal_id = "term-gone".to_string();
    let mut open_live = rec("open-live-term");
    open_live.terminal_id = "term-live".to_string();
    store.record_open(closed_live);
    store.record_open(closed_live_older);
    store.record_open(closed_gone);
    store.record_open(open_live);
    store.record_close("closed-live-term", "pty-exit");
    store.record_close("closed-live-term-older", "pty-exit");
    store.record_close("closed-gone-term", "pty-exit");

    let now = Utc::now().timestamp_millis();
    // Age both closed rows past retention and the open row past stale.
    age_persisted_records(&store, &path, |m| {
        m.get_mut("closed-live-term").unwrap().closed_at = Some(now - CLOSED_RETENTION_MS - 1_000);
        // An OLDER closed sibling under the same live terminal — only the
        // newest closed row per live terminal is retention-exempt.
        m.get_mut("closed-live-term-older").unwrap().closed_at =
            Some(now - CLOSED_RETENTION_MS - 2_000);
        m.get_mut("closed-gone-term").unwrap().closed_at = Some(now - CLOSED_RETENTION_MS - 1_000);
        m.get_mut("open-live-term").unwrap().last_seen_at = now - OPEN_STALE_MS - 1_000;
    });

    let store = SessionLifecycleStore::open(&path).unwrap();
    let live: HashSet<String> = ["term-live".to_string()].into_iter().collect();
    store.prune(now, &live);

    let m: HashMap<String, TerminalSessionRecord> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        m.contains_key("closed-live-term"),
        "newest closed row survives while its terminal lives"
    );
    assert!(
        !m.contains_key("closed-live-term-older"),
        "older closed sibling under the same live terminal ages out"
    );
    assert!(
        !m.contains_key("closed-gone-term"),
        "terminal gone → pruned after retention"
    );
    assert!(
        m.contains_key("open-live-term"),
        "stale-open row with a live terminal survives"
    );

    // Once the terminal is gone too, the same aged rows are pruned.
    store.prune(now, &HashSet::new());
    let m: HashMap<String, TerminalSessionRecord> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        !m.contains_key("closed-live-term"),
        "pruned once its terminal is gone"
    );
    assert!(!m.contains_key("open-live-term"));
}

#[test]
fn corrupt_file_starts_empty() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    std::fs::write(&path, b"{not valid json").unwrap();
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.open_records().is_empty());
    // Still usable after recovery.
    store.record_open(rec("sess-1"));
    assert_eq!(store.open_records().len(), 1);
}

/// Item-1: one malformed record must NOT discard the whole store. A real
/// boot loads a registry whose object shape is intact but one entry fails
/// to deserialize (schema skew / partial corruption); the load must KEEP
/// the valid rows and warn+drop only the bad one — the original
/// whole-map deserialize wiped everything on the first bad value, silently
/// losing every restorable on-screen session.
#[test]
fn malformed_record_drops_only_that_row_and_keeps_the_rest() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    // Two valid records + one malformed (wrong type for `zoneIndex`, which
    // is a required `i32`, so `from_value` rejects just this entry).
    let file = r#"{
            "good-1": {
                "claudeSessionId":"good-1","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":1,"title":"Good 1","terminalId":"term-1",
                "openedAt":100,"lastSeenAt":200,"state":"open","closedAt":null,"closeReason":null
            },
            "bad-1": {
                "claudeSessionId":"bad-1","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":"NOT-AN-INT","title":"Bad","terminalId":"term-bad",
                "openedAt":100,"lastSeenAt":200,"state":"open","closedAt":null,"closeReason":null
            },
            "good-2": {
                "claudeSessionId":"good-2","configDir":null,"workingDir":"C:/repo",
                "pageId":"default","zoneIndex":2,"title":"Good 2","terminalId":"term-2",
                "openedAt":300,"lastSeenAt":400,"state":"open","closedAt":null,"closeReason":null
            }
        }"#;
    std::fs::write(&path, file).unwrap();

    let store = SessionLifecycleStore::open(&path).unwrap();
    let mut ids: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["good-1".to_string(), "good-2".to_string()],
        "the two valid rows must survive; only the malformed row is dropped"
    );

    // The dropped raw entry is preserved in a `.corrupt` side-car.
    let sidecar = path.with_extension("json.corrupt");
    assert!(
        sidecar.exists(),
        "a .corrupt side-car must capture the drop"
    );
    let side: HashMap<String, serde_json::Value> =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert!(side.contains_key("bad-1"), "side-car holds the dropped row");
    assert!(!side.contains_key("good-1"), "side-car excludes good rows");

    // Store stays usable; a fresh write persists the kept-plus-new set.
    store.record_open(rec("new-1"));
    let store = SessionLifecycleStore::open(&path).unwrap();
    let mut ids2: Vec<String> = store
        .open_records()
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids2.sort();
    assert_eq!(
        ids2,
        vec![
            "good-1".to_string(),
            "good-2".to_string(),
            "new-1".to_string()
        ],
    );
}

// --- classify() — every branch -----------------------------------------

/// Every case below the marker predates the `confirmed` dimension and
/// asserts the behaviour of a CONFIRMED record (a provider session really
/// started). Pinning that one dimension here keeps those cases reading as
/// the liveness matrix they are; the never-confirmed dimension is covered
/// separately by `classify_never_confirmed_*`.
fn classify_confirmed(
    live_is_alive: Option<bool>,
    claude_present: bool,
    consecutive_dead: u32,
    consecutive_no_match: u32,
    snapshot_ok: bool,
    restore_pending: bool,
) -> PollAction {
    classify(
        live_is_alive,
        claude_present,
        consecutive_dead,
        consecutive_no_match,
        snapshot_ok,
        restore_pending,
        true,
        // Terminal plane: this helper exists to exercise the PTY-bound
        // arms, which is every arm the worker plane bypasses.
        WorkerPlane::NotWorker,
    )
}

#[test]
fn classify_skip_on_snapshot_failure() {
    // snapshot_ok=false dominates every other input.
    assert_eq!(
        classify_confirmed(Some(false), false, 5, 0, false, false),
        PollAction::Skip
    );
    assert_eq!(
        classify_confirmed(Some(true), true, 0, 0, false, false),
        PollAction::Skip
    );
    assert_eq!(
        classify_confirmed(None, false, 0, 0, false, false),
        PollAction::Skip
    );
    // Even a no-match streak past the orphan threshold must Skip (not
    // CloseNoTerminal) when the snapshot failed.
    assert_eq!(
        classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 5, false, false),
        PollAction::Skip
    );
}

#[test]
fn classify_no_match_waits_below_orphan_ticks() {
    // A record matching no live terminal accumulates NoMatchWait ticks
    // for every tick strictly below NO_TERMINAL_ORPHAN_TICKS — it must
    // NOT close on a brief registration race.
    for prior in 0..NO_TERMINAL_ORPHAN_TICKS {
        assert_eq!(
            classify_confirmed(None, false, 0, prior, true, false),
            PollAction::NoMatchWait,
            "no match, {prior} prior no-match ticks must NoMatchWait"
        );
    }
    // claude_present/dead-tick inputs are irrelevant to the no-match arm.
    assert_eq!(
        classify_confirmed(None, true, 9, 0, true, false),
        PollAction::NoMatchWait
    );
}

#[test]
fn classify_no_match_closes_no_terminal_after_orphan_ticks() {
    // Once the no-match streak reaches the threshold the record is an
    // orphan — close it with the (non-restorable) "no-terminal" reason.
    // With a 45s poll the close lands on the 4th consecutive tick ≈ 3min.
    assert_eq!(
        classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS, true, false),
        PollAction::CloseNoTerminal
    );
    assert_eq!(
        classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 5, true, false),
        PollAction::CloseNoTerminal
    );
}

#[test]
fn classify_close_on_dead_shell() {
    // A dead pty closes regardless of claude_present — the unambiguous
    // confident-dead signal.
    assert_eq!(
        classify_confirmed(Some(false), false, 0, 0, true, false),
        PollAction::Close
    );
    assert_eq!(
        classify_confirmed(Some(false), true, 0, 0, true, false),
        PollAction::Close
    );
}

#[test]
fn classify_keepalive_when_claude_present() {
    // Claude present in the inclusive subtree ⇒ KeepAlive immediately —
    // even with zero prior dead ticks (the idle-agent bug) and even after
    // a long prior claude-absent streak (claude came back).
    assert_eq!(
        classify_confirmed(Some(true), true, 0, 0, true, false),
        PollAction::KeepAlive
    );
    assert_eq!(
        classify_confirmed(Some(true), true, 9, 0, true, false),
        PollAction::KeepAlive
    );
}

#[test]
fn classify_idle_agent_session_is_kept_alive() {
    // Regression for the live incident (2026-06-19): an idle agent /
    // gate-continuation session whose tracked PID *is* claude spawns zero
    // children while parked on a gate. Under the old `descendant_count > 0`
    // heuristic this read as dead and was closed `poll-dead` in ~135s while
    // perfectly alive. With `claude_present` the answer is KeepAlive on the
    // very first tick, forever, no matter how long it idles.
    assert_eq!(
        classify_confirmed(Some(true), true, 0, 0, true, false),
        PollAction::KeepAlive
    );
    assert_eq!(
        classify_confirmed(
            Some(true),
            true,
            LIVE_SHELL_DEAD_TICKS + 100,
            0,
            true,
            false
        ),
        PollAction::KeepAlive
    );
}

#[test]
fn classify_operator_quit_claude_closes_after_debounce() {
    // The preserved bare-shell cleanup: operator quits claude, the shell
    // PID stays alive but no claude remains in its subtree → NeedsConfirm
    // through the debounce, then Close. (prior < LIVE_SHELL_DEAD_TICKS is
    // NeedsConfirm, NOT Close — the plan's test-plan "prior=2 ⇒ Close" bullet
    // predated keeping the 3-tick debounce; Change #2 keeps it, so Close
    // requires the full streak.)
    assert_eq!(
        classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS - 1, 0, true, false),
        PollAction::NeedsConfirm
    );
    assert_eq!(
        classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, false),
        PollAction::Close
    );
}

#[test]
fn classify_needs_confirm_while_below_live_shell_dead_ticks() {
    // A live shell with NO claude present stays in NeedsConfirm for every
    // tick strictly below LIVE_SHELL_DEAD_TICKS — it must NOT close on a
    // brief blip (a transient snapshot miss / in-flight relaunch).
    for prior in 0..LIVE_SHELL_DEAD_TICKS {
        assert_eq!(
            classify_confirmed(Some(true), false, prior, 0, true, false),
            PollAction::NeedsConfirm,
            "live shell, claude absent, {prior} prior ticks must NeedsConfirm"
        );
    }
    // Specifically: the second tick (prior == 1) no longer closes.
    assert_eq!(
        classify_confirmed(Some(true), false, 1, 0, true, false),
        PollAction::NeedsConfirm
    );
}

#[test]
fn classify_close_only_after_live_shell_dead_ticks_reached() {
    // Only once we've accumulated LIVE_SHELL_DEAD_TICKS consecutive
    // claude-absent ticks does a live shell finally close (operator quit
    // claude; the bare shell PID lingers).
    assert_eq!(
        classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, false),
        PollAction::Close
    );
    assert_eq!(
        classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS + 5, 0, true, false),
        PollAction::Close
    );
}

#[test]
fn classify_restore_pending_never_closes() {
    // The incident shape: a restored pane whose resume silently failed.
    // Dead pty (the restore shell died / was never matched) → Skip, NOT
    // Close — the durable `open` record must survive for the next attempt.
    assert_eq!(
        classify_confirmed(Some(false), false, 0, 0, true, true),
        PollAction::Skip
    );
    // Plain shell with no claude present, even past the debounce ticks
    // (the exact poll-dead flip the incident hit) → Skip.
    assert_eq!(
        classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS, 0, true, true),
        PollAction::Skip
    );
    assert_eq!(
        classify_confirmed(Some(true), false, LIVE_SHELL_DEAD_TICKS + 5, 0, true, true),
        PollAction::Skip
    );
    // Below the debounce it must not even accumulate NeedsConfirm ticks.
    assert_eq!(
        classify_confirmed(Some(true), false, 0, 0, true, true),
        PollAction::Skip
    );
    // No matching pty → Skip, NOT NoMatchWait: a mid-restore row keeps
    // its OLD terminal_id until the re-assert and must never accumulate
    // no-match ticks toward an orphan close.
    assert_eq!(
        classify_confirmed(None, false, 0, 0, true, true),
        PollAction::Skip
    );
    // Even a streak past the orphan threshold must not close while the
    // restore-pending marker is set.
    assert_eq!(
        classify_confirmed(None, false, 0, NO_TERMINAL_ORPHAN_TICKS + 1, true, true),
        PollAction::Skip
    );
}

#[test]
fn classify_restore_pending_passes_through_confident_alive() {
    // A confidently-alive session (claude present) classifies KeepAlive
    // even while restore-pending — the caller uses this to self-heal a
    // stale marker.
    assert_eq!(
        classify_confirmed(Some(true), true, 0, 0, true, true),
        PollAction::KeepAlive
    );
}

/// Item 3 regression: 39 of 42 records on the primary read
/// `closed/"poll-dead"` while their PTY was demonstrably alive. Every PTY
/// gets a provisional record at spawn; a bare PowerShell pane never runs a
/// provider, so `claude_present` is false forever and the debounce
/// inevitably expires. Such a record must never be called `poll-dead`.
#[test]
fn classify_never_confirmed_bare_shell_is_never_poll_dead() {
    // The exact observed shape: live shell, no claude, debounce expired.
    assert_eq!(
        classify(
            Some(true),
            false,
            LIVE_SHELL_DEAD_TICKS,
            0,
            true,
            false,
            false,
            WorkerPlane::NotWorker,
        ),
        PollAction::CloseNeverStarted,
    );
    assert_eq!(
        classify(
            Some(true),
            false,
            LIVE_SHELL_DEAD_TICKS + 5,
            0,
            true,
            false,
            false,
            WorkerPlane::NotWorker,
        ),
        PollAction::CloseNeverStarted,
    );
    // A dead pty for a never-started record is likewise nothing dying.
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            true,
            false,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::CloseNeverStarted,
    );
    // The verification case: a bare terminal after two poll cycles has not
    // reached the debounce, so it is still merely NeedsConfirm — and under
    // no input does it produce Close.
    for prior in 0..=(LIVE_SHELL_DEAD_TICKS + 5) {
        assert_ne!(
            classify(
                Some(true),
                false,
                prior,
                0,
                true,
                false,
                false,
                WorkerPlane::NotWorker
            ),
            PollAction::Close,
            "a never-confirmed record must never classify poll-dead ({prior} ticks)"
        );
    }
}

/// The guard must be narrow: it only rewrites the `poll-dead` close. A
/// CONFIRMED record keeps closing `poll-dead` (the operator-quit-claude
/// cleanup), and the never-confirmed guard never resurrects a record or
/// suppresses the orphan / keep-alive arms.
#[test]
fn classify_never_confirmed_leaves_other_arms_intact() {
    // Confirmed + same inputs ⇒ still the real poll-dead close.
    assert_eq!(
        classify(
            Some(true),
            false,
            LIVE_SHELL_DEAD_TICKS,
            0,
            true,
            false,
            true,
            WorkerPlane::NotWorker,
        ),
        PollAction::Close,
    );
    // An unconfirmed record with a live claude is a real session mid-bind
    // (the hook lands within seconds) — KeepAlive, not a close.
    assert_eq!(
        classify(
            Some(true),
            true,
            0,
            0,
            true,
            false,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::KeepAlive,
    );
    // Orphan close keeps its own already-non-restorable reason.
    assert_eq!(
        classify(
            None,
            false,
            0,
            NO_TERMINAL_ORPHAN_TICKS,
            true,
            false,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::CloseNoTerminal,
    );
    // Uncertainty still dominates: snapshot failure and restore-pending.
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            false,
            false,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::Skip,
    );
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            true,
            true,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::Skip,
    );
}

/// The exact observed shape: a page whose restore already landed several
/// records `terminal-only` (confirmed, no transcript) recreates a blank
/// terminal for each on every first activation; each one idles out and
/// re-classifies `Close`. Before this fix that closed `"poll-dead"`
/// (restorable), handing the next restore pass the identical unresumable
/// record and recreating the identical blank terminal — an unbounded
/// "many panes, no sessions" accumulation, and (on a machine near the
/// thread ceiling) a burst of resource-guard refusals from one tab click,
/// one per phantom pane.
#[test]
fn close_reason_for_dead_shell_retires_terminal_only_tiles_non_restorably() {
    assert_eq!(
        close_reason_for_dead_shell(Some(RESTORE_TIER_TERMINAL_ONLY)),
        "terminal-only-idle",
    );
    // `restorable_records` only grants grace to "poll-dead" / "pty-exit" —
    // pin that this reason is neither, so the fix does not also require
    // touching that allowlist.
    assert_ne!(
        close_reason_for_dead_shell(Some(RESTORE_TIER_TERMINAL_ONLY)),
        "poll-dead"
    );
    assert_ne!(
        close_reason_for_dead_shell(Some(RESTORE_TIER_TERMINAL_ONLY)),
        "pty-exit"
    );
}

/// The guard stays narrow: every OTHER restore tier (a landed resume, a
/// still-in-flight/failed restore, or no restore ever recorded) is a
/// record that might genuinely still be alive right now — those must keep
/// closing `"poll-dead"` so a real dead session stays restorable.
#[test]
fn close_reason_for_dead_shell_leaves_other_tiers_as_poll_dead() {
    assert_eq!(
        close_reason_for_dead_shell(Some(RESTORE_TIER_RESUMED)),
        "poll-dead"
    );
    assert_eq!(
        close_reason_for_dead_shell(Some(RESTORE_TIER_FAILED)),
        "poll-dead"
    );
    assert_eq!(close_reason_for_dead_shell(None), "poll-dead");
}

/// The regression that would have failed on runner#871.
///
/// A Phase-2 orchestration worker's lifecycle record carries
/// `terminal_id == task_run_id`, which matches nothing in the live-terminal
/// index — so the poll sees `live_is_alive: None` on EVERY tick and the
/// orphan debounce closed its coord session row after
/// `NO_TERMINAL_ORPHAN_TICKS`, while the worker was still running. A
/// registered worker must survive an unbounded no-match streak.
#[test]
fn classify_registered_worker_never_orphan_closes() {
    for streak in [
        0,
        NO_TERMINAL_ORPHAN_TICKS,
        NO_TERMINAL_ORPHAN_TICKS + 1,
        NO_TERMINAL_ORPHAN_TICKS + 5,
        NO_TERMINAL_ORPHAN_TICKS + 100,
    ] {
        assert_eq!(
            classify(
                None,
                false,
                0,
                streak,
                true,
                false,
                false,
                WorkerPlane::Registered
            ),
            PollAction::KeepAlive,
            "a registered worker must never orphan-close ({streak} no-match ticks)"
        );
    }
    // Confirmed or not, restore-pending or not, snapshot ok or not: the
    // terminal plane's evidence is meaningless for a record with no PTY.
    for confirmed in [true, false] {
        for restore_pending in [true, false] {
            for snapshot_ok in [true, false] {
                assert_eq!(
                    classify(
                        None,
                        false,
                        0,
                        NO_TERMINAL_ORPHAN_TICKS + 1,
                        snapshot_ok,
                        restore_pending,
                        confirmed,
                        WorkerPlane::Registered
                    ),
                    PollAction::KeepAlive,
                );
            }
        }
    }
}

/// The other half of the fix: suppressing the close outright would LEAK.
/// Nothing in `orchestration_loop` ever closes a worker's lifecycle row —
/// this is the only path that retires one — so a worker that has left the
/// `SessionManager` must still be closed, non-restorably.
#[test]
fn classify_gone_worker_is_still_orphan_closed() {
    assert_eq!(
        classify(None, false, 0, 0, true, false, false, WorkerPlane::Gone),
        PollAction::CloseNoTerminal,
    );
    assert_eq!(
        classify(
            None,
            false,
            0,
            NO_TERMINAL_ORPHAN_TICKS + 5,
            true,
            false,
            true,
            WorkerPlane::Gone
        ),
        PollAction::CloseNoTerminal,
    );
}

/// Uncertainty never closes: a worker record whose plane could not be
/// consulted (no `SessionManager` in app state) is skipped, not retired —
/// the same posture as a failed process snapshot.
#[test]
fn classify_worker_plane_unknown_is_skip() {
    for snapshot_ok in [true, false] {
        assert_eq!(
            classify(
                None,
                false,
                0,
                NO_TERMINAL_ORPHAN_TICKS + 5,
                snapshot_ok,
                false,
                false,
                WorkerPlane::Unknown
            ),
            PollAction::Skip,
        );
    }
}

/// `NotWorker` must leave the terminal plane byte-identical. Every arm of
/// the pre-existing decision core, re-asserted through the new parameter.
#[test]
fn classify_not_worker_preserves_every_terminal_plane_arm() {
    // Orphan debounce, then the orphan close.
    for prior in 0..NO_TERMINAL_ORPHAN_TICKS {
        assert_eq!(
            classify(
                None,
                false,
                0,
                prior,
                true,
                false,
                true,
                WorkerPlane::NotWorker
            ),
            PollAction::NoMatchWait,
        );
    }
    assert_eq!(
        classify(
            None,
            false,
            0,
            NO_TERMINAL_ORPHAN_TICKS,
            true,
            false,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::CloseNoTerminal,
    );
    // Live claude in the subtree — KeepAlive unconditionally.
    assert_eq!(
        classify(
            Some(true),
            true,
            0,
            0,
            true,
            false,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::KeepAlive,
    );
    // No claude: debounce, then the real poll-dead close.
    assert_eq!(
        classify(
            Some(true),
            false,
            0,
            0,
            true,
            false,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::NeedsConfirm,
    );
    assert_eq!(
        classify(
            Some(true),
            false,
            LIVE_SHELL_DEAD_TICKS,
            0,
            true,
            false,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::Close,
    );
    // Dead pty.
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            true,
            false,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::Close,
    );
    // Never-confirmed guard still rewrites ONLY the poll-dead close...
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            true,
            false,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::CloseNeverStarted,
    );
    // ...and still leaves the orphan close alone. Widening it here was the
    // 2026-07-26 plan's proposed fix; the vet refuted it, because
    // `CloseNeverStarted` is a close too and closes the coord row all the same.
    assert_eq!(
        classify(
            None,
            false,
            0,
            NO_TERMINAL_ORPHAN_TICKS,
            true,
            false,
            false,
            WorkerPlane::NotWorker
        ),
        PollAction::CloseNoTerminal,
    );
    // Uncertainty still dominates.
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            false,
            false,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::Skip,
    );
    assert_eq!(
        classify(
            Some(false),
            false,
            0,
            0,
            true,
            true,
            true,
            WorkerPlane::NotWorker
        ),
        PollAction::Skip,
    );
}

/// `never-started` must be non-restorable — the whole point of not calling
/// it `poll-dead`, which buys a [`RESTORABLE_POLL_DEAD_MS`] restore grace.
#[test]
fn never_started_close_is_not_restorable() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("bare-shell"));
    store.record_close("bare-shell", "never-started");

    assert!(
        store.restorable_records(None, true).is_empty(),
        "a never-started (bare shell) record must never be a restore candidate"
    );
}

#[test]
fn no_terminal_close_is_not_restorable() {
    let _amb = crate::test_env::isolated_ambient();
    // The poll's orphan close ("no-terminal") must NOT re-qualify for
    // restore — only "pty-exit"/"poll-dead" closes get a grace window.
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("orphan-sess"));
    store.record_close("orphan-sess", "no-terminal");

    assert!(
        store.restorable_records(None, true).is_empty(),
        "a no-terminal close (even seconds old) must not be restorable"
    );
}

#[test]
fn mark_and_clear_restore_pending_round_trip_and_persist() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-1"));
    assert!(store.open_records()[0].restore_pending_at.is_none());

    store.mark_restore_pending("sess-1", true);
    assert!(store.open_records()[0].restore_pending_at.is_some());

    // Durable across a "restart" — the marker must survive a frontend /
    // process crash mid-restore.
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.open_records()[0].restore_pending_at.is_some());

    store.clear_restore_pending("sess-1");
    assert!(store.open_records()[0].restore_pending_at.is_none());
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.open_records()[0].restore_pending_at.is_none());

    // Absent ids are no-ops (no panic).
    store.mark_restore_pending("ghost", true);
    store.clear_restore_pending("ghost");
    // Double-clear is a no-op.
    store.clear_restore_pending("sess-1");
}

/// Plan item 9(b): the rendered verdict must AGE OUT. A marker whose
/// resume verification silently died used to report "pending" forever,
/// which is a worse lie than the pessimistic `failed` the pending
/// rendering exists to soften.
#[test]
fn describe_restore_status_ages_a_stale_pending_marker_out() {
    let now = 1_700_000_000_000i64;

    // --- `failed` tier (the mark_restore_pending stamp) ---
    assert_eq!(
        describe_restore_status(Some(RESTORE_TIER_FAILED), Some(now - 1_000), now),
        "pending (not yet confirmed)",
        "a second-old marker is a restore in flight"
    );
    assert_eq!(
        describe_restore_status(
            Some(RESTORE_TIER_FAILED),
            Some(now - RESTORE_PENDING_TTL_MS),
            now
        ),
        "pending (not yet confirmed)",
        "exactly AT the TTL is still in flight — the boundary is exclusive"
    );
    assert_eq!(
        describe_restore_status(
            Some(RESTORE_TIER_FAILED),
            Some(now - RESTORE_PENDING_TTL_MS - 1),
            now
        ),
        "failed (verification timed out)",
        "one ms past the TTL and the verification is not coming back"
    );

    // --- no tier at all: an ancient marker is just as stale ---
    assert_eq!(
        describe_restore_status(None, Some(now - 1_000), now),
        "pending (not yet confirmed)"
    );
    assert_eq!(
        describe_restore_status(None, Some(now - RESTORE_PENDING_TTL_MS - 1), now),
        "failed (verification timed out)"
    );

    // --- the arms the TTL must NOT touch ---
    assert_eq!(
        describe_restore_status(Some(RESTORE_TIER_FAILED), None, now),
        "failed",
        "marker already cleared without an upgrade — a real failure verdict"
    );
    assert_eq!(
        describe_restore_status(
            Some(RESTORE_TIER_RESUMED),
            Some(now - RESTORE_PENDING_TTL_MS - 1),
            now
        ),
        "resumed",
        "a landed restore is not re-litigated by an old marker"
    );
    assert_eq!(
        describe_restore_status(None, None, now),
        "not-restored",
        "never restored is not a failed restore"
    );

    // Clock skew: a marker stamped in the FUTURE must read as in flight,
    // not wrap into a huge age and report a timeout that never happened.
    assert_eq!(
        describe_restore_status(Some(RESTORE_TIER_FAILED), Some(now + 60_000), now),
        "pending (not yet confirmed)"
    );
}

/// Plan item 9(a): a close is the end of the road for a restore, so the
/// pending marker must be reaped there. Driven through the PUBLIC closer,
/// not by poking fields.
#[test]
fn record_close_reaps_the_restore_pending_marker_and_demotes_failed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-1"));
    store.mark_restore_pending("sess-1", true);
    assert_eq!(
        store.get("sess-1").unwrap().restore_tier.as_deref(),
        Some(RESTORE_TIER_FAILED),
        "precondition: the pessimistic stamp is in place"
    );

    store.record_close("sess-1", "pty-exit");

    let closed = store.get("sess-1").unwrap();
    assert_eq!(closed.state, "closed");
    assert!(
        closed.restore_pending_at.is_none(),
        "a closed record's restore can never be retried — the marker must go"
    );
    assert_eq!(
        closed.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY),
        "the tier must move too: ?include=failed selects on the TIER, so a \
         closed row keeping `failed` is still reported as a broken restore"
    );
    assert_eq!(
        describe_restore_status(
            closed.restore_tier.as_deref(),
            closed.restore_pending_at,
            Utc::now().timestamp_millis()
        ),
        "terminal-only",
        "and the rendered verdict stops claiming a restore is in flight"
    );

    // Durable across a reload — the reap is a real write, not a read-time
    // projection.
    let store = SessionLifecycleStore::open(&path).unwrap();
    let reloaded = store.get("sess-1").unwrap();
    assert!(reloaded.restore_pending_at.is_none());
    assert_eq!(
        reloaded.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY)
    );
}

/// The reap clears the MARKER on every close, but demotes only the
/// pessimistic `failed` tier: a landed restore keeps its verdict, and a
/// record that was never restored must not acquire one.
#[test]
fn record_close_does_not_demote_a_resumed_or_absent_tier() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // A landed restore: mark → clear promotes `failed` → `resumed`.
    store.record_open(rec("landed"));
    store.mark_restore_pending("landed", true);
    store.clear_restore_pending("landed");
    assert_eq!(
        store.get("landed").unwrap().restore_tier.as_deref(),
        Some(RESTORE_TIER_RESUMED),
        "precondition: the promotion landed"
    );
    store.record_close("landed", "explicit");
    assert_eq!(
        store.get("landed").unwrap().restore_tier.as_deref(),
        Some(RESTORE_TIER_RESUMED),
        "a close must not rewrite the verdict of a restore that DID land"
    );

    // A terminal-only restore is already the demotion target — untouched.
    let mut ghost = rec("ghost");
    ghost.terminal_id = "term-ghost".to_string();
    store.record_open(ghost);
    store.mark_restored_from_boot("ghost", RESTORE_TIER_TERMINAL_ONLY);
    store.record_close("ghost", "pty-exit");
    assert_eq!(
        store.get("ghost").unwrap().restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY)
    );

    // Never restored: the close must not invent a tier.
    let mut plain = rec("plain");
    plain.terminal_id = "term-plain".to_string();
    store.record_open(plain);
    store.record_close("plain", "explicit");
    let closed = store.get("plain").unwrap();
    assert_eq!(closed.restore_tier, None, "no restore was ever recorded");
    assert!(closed.restore_pending_at.is_none());
}

/// `record_open`'s phantom-sibling eviction is a close writer too — it
/// stamps `state = "closed"` on a row it never routes through
/// `record_close`, so it owes the same reap.
#[test]
fn record_open_eviction_of_an_unconfirmed_sibling_reaps_the_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // An UNCONFIRMED phantom holding a restore-pending marker…
    store.record_open(rec("phantom"));
    store.mark_restore_pending("phantom", true);

    // …evicted when a new AUTHORITATIVE session binds the same terminal.
    let mut incoming = rec("real");
    incoming.origin = Some(ORIGIN_AUTHORITATIVE.to_string());
    store.record_open(incoming);

    let evicted = store.get("phantom").unwrap();
    assert_eq!(evicted.state, "closed");
    assert_eq!(evicted.close_reason.as_deref(), Some("superseded"));
    assert!(
        evicted.restore_pending_at.is_none(),
        "the evicted phantom's restore can never be retried"
    );
    assert_eq!(
        evicted.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY),
        "and it must not linger in the ?include=failed bucket"
    );
}

/// The second `record_open` close writer: a prior CONFIRMED session retired
/// when a new confirmed one reuses its terminal.
#[test]
fn record_open_terminal_reuse_supersede_reaps_the_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    let mut prior = rec("prior");
    prior.confirmed_at = Some(1_000);
    store.record_open(prior);
    store.mark_restore_pending("prior", true);

    let mut next = rec("next");
    next.confirmed_at = Some(2_000);
    store.record_open(next);

    let retired = store.get("prior").unwrap();
    assert_eq!(retired.state, "closed");
    assert_eq!(
        retired.close_reason.as_deref(),
        Some("superseded-terminal-reuse")
    );
    assert!(retired.restore_pending_at.is_none());
    assert_eq!(
        retired.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY)
    );
}

/// The fourth close writer: the boot-time collision repair. It closes the
/// losing rows in bulk and compacts, so the reap has to survive a reload.
#[test]
fn repair_terminal_id_collisions_reaps_the_restore_pending_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");

    let mut keeper = fixture_rec("keeper", "open", 3_000, None, None);
    keeper.terminal_id = "shared".to_string();
    keeper.confirmed_at = Some(3_000);
    let mut loser = fixture_rec("loser", "open", 1_000, None, None);
    loser.terminal_id = "shared".to_string();
    loser.restore_pending_at = Some(1_000);
    loser.restored_from_boot_at = Some(1_000);
    loser.restore_tier = Some(RESTORE_TIER_FAILED.to_string());
    write_fixture(&path, vec![keeper, loser]);

    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(store.repair_terminal_id_collisions(), 1);

    let collapsed = store.get("loser").unwrap();
    assert_eq!(collapsed.state, "closed");
    assert!(
        collapsed.restore_pending_at.is_none(),
        "a collapsed row is closed — its restore can never be retried"
    );
    assert_eq!(
        collapsed.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY)
    );

    // The repair compacts rather than appending deltas — prove the reap is
    // in the compacted snapshot.
    let store = SessionLifecycleStore::open(&path).unwrap();
    let reloaded = store.get("loser").unwrap();
    assert!(reloaded.restore_pending_at.is_none());
    assert_eq!(
        reloaded.restore_tier.as_deref(),
        Some(RESTORE_TIER_TERMINAL_ONLY)
    );
}

#[test]
fn record_open_preserves_restore_pending_marker() {
    // The restore path re-asserts the open record under the fresh terminal
    // id AFTER marking restore-pending — the re-assert must not wipe the
    // marker (only a verified handshake / confident-alive poll clears it).
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("sess-1"));
    store.mark_restore_pending("sess-1", true);

    let mut r2 = rec("sess-1");
    r2.terminal_id = "term-new".to_string();
    store.record_open(r2);

    let open = store.open_records();
    assert_eq!(open[0].terminal_id, "term-new", "re-assert refreshed");
    assert!(
        open[0].restore_pending_at.is_some(),
        "record_open must preserve the restore-pending marker"
    );
}

/// Build a fully-specified fixture record (explicit timestamps, unlike
/// `rec` whose timestamps `record_open` overwrites).
fn fixture_rec(
    id: &str,
    state: &str,
    last_seen_at: i64,
    closed_at: Option<i64>,
    close_reason: Option<&str>,
) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: id.to_string(),
        config_dir: None,
        working_dir: Some("C:/repo".to_string()),
        page_id: "default".to_string(),
        zone_index: 0,
        title: Some(id.to_string()),
        terminal_id: format!("term-{id}"),
        opened_at: last_seen_at - 1_000,
        last_seen_at,
        state: state.to_string(),
        closed_at,
        close_reason: close_reason.map(str::to_string),
        provider: DEFAULT_PROVIDER.to_string(),
        origin: None,
        restore_pending_at: None,
        awaiting_account_since: None,
        confirmed_at: None,
        handle: None,
        account_label: None,
        account_wrapper: None,
        session_name: None,
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
    }
}

/// Write a registry fixture file directly (bypassing `record_open`'s
/// now-stamping) so tests control every timestamp.
fn write_fixture(path: &Path, recs: Vec<TerminalSessionRecord>) {
    let m: HashMap<String, TerminalSessionRecord> = recs
        .into_iter()
        .map(|r| (r.claude_session_id.clone(), r))
        .collect();
    std::fs::write(path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
}

fn restorable_ids(
    store: &SessionLifecycleStore,
    prior_marker_at: Option<i64>,
    boot_was_clean: bool,
) -> Vec<String> {
    let mut ids: Vec<String> = store
        .restorable_records(prior_marker_at, boot_was_clean)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    ids
}

/// Item-1 verification fixture (the on-page repro): two in-grace
/// pty-exit rows (exactly what a graceful shutdown writes for on-screen
/// panes), one explicit close, and one stale `open` ghost whose terminal
/// died 72h before shutdown. After a clean restart only the two pty-exit
/// rows may restore — the ghost spawns no pane.
#[test]
fn restorable_records_exclude_stale_open_ghost_after_clean_restart() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64; // registry's last moment of life
                                          // restart one minute later — irrelevant: admission is anchor-relative.
    write_fixture(
        &path,
        vec![
            fixture_rec(
                "fixture-onscreen-0001",
                "closed",
                shutdown,
                Some(shutdown),
                Some("pty-exit"),
            ),
            fixture_rec(
                "fixture-onscreen-0002",
                "closed",
                shutdown,
                Some(shutdown),
                Some("pty-exit"),
            ),
            fixture_rec(
                "fixture-explicit-0003",
                "closed",
                shutdown,
                Some(shutdown),
                Some("explicit"),
            ),
            fixture_rec(
                "fixture-ghost-0004",
                "open",
                shutdown - 72 * 3_600_000,
                None,
                None,
            ),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let expected = vec![
        "fixture-onscreen-0001".to_string(),
        "fixture-onscreen-0002".to_string(),
    ];
    // With the clean-shutdown marker anchoring the registry…
    assert_eq!(restorable_ids(&store, Some(shutdown), true), expected);
    // …and even without it: the pty-exit closes already anchor the
    // registry at the shutdown instant, so the 72h ghost stays out.
    assert_eq!(restorable_ids(&store, None, true), expected);
}

/// Anchored recency is downtime-proof: open rows fresh AT THE CRASH
/// restore even when the next boot happens hours later (a wall-clock
/// `now - last_seen <= grace` rule would restore nothing), while the
/// stale ghost stays excluded.
#[test]
fn restorable_records_open_rows_survive_long_downtime_anchor_relative() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let crash = 1_700_000_000_000_i64;
    // booted 6 hours later — irrelevant: admission is anchor-relative.
    write_fixture(
        &path,
        vec![
            fixture_rec("fresh-a", "open", crash, None, None),
            fixture_rec("fresh-b", "open", crash - 30_000, None, None),
            fixture_rec("ghost", "open", crash - 72 * 3_600_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    // The crashed process's marker (`clean:false`, stamped at ITS boot,
    // a day earlier) must not shrink the anchor below the fresh rows.
    let prior_marker_at = Some(crash - 86_400_000);
    // Unclean boot: the crashed process's own marker is excluded from the
    // anchor; the dense crash cohort supplies it.
    assert_eq!(
        restorable_ids(&store, prior_marker_at, false),
        vec!["fresh-a".to_string(), "fresh-b".to_string()],
        "fresh-at-crash open rows restore after multi-hour downtime; ghost excluded"
    );
}

/// Plan `2026-10-04-runner-session-roster-restore-picker`, Phase 1: the
/// CLOSED arms are anchor-relative too. A graceful stop stamps every PTY
/// `pty-exit` at the shutdown instant; a rebuild that boots 3 h later must
/// still restore all of them (a wall-clock `now - closed_at` rule restored
/// NOTHING past 10 minutes), while
/// (b) a `pty-exit` close 30 min BEFORE shutdown stays stale,
/// (c) a `user-close` / `no-terminal` close is still never restored, and
/// (d) a finished row is still excluded — on BOTH boot kinds.
#[test]
fn restorable_records_closed_rows_are_anchor_relative_across_long_rebuild() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    // (The boot happens 3 h after `shutdown`; nothing below reads a clock.)
    let mut finished = fixture_rec(
        "finished-at-shutdown",
        "closed",
        shutdown,
        Some(shutdown),
        Some("pty-exit"),
    );
    finished.finished_at = Some(shutdown - 60_000);
    write_fixture(
        &path,
        vec![
            // (a) the graceful-stop cohort.
            fixture_rec(
                "graceful-a",
                "closed",
                shutdown,
                Some(shutdown),
                Some("pty-exit"),
            ),
            fixture_rec(
                "graceful-b",
                "closed",
                shutdown - 2_000,
                Some(shutdown - 1_000),
                Some("pty-exit"),
            ),
            // An idle-but-live shell the poll closed just before shutdown.
            fixture_rec(
                "poll-dead-recent",
                "closed",
                shutdown - 120_000,
                Some(shutdown - 60_000),
                Some("poll-dead"),
            ),
            // (b) closed 30 min before shutdown — stale relative to the anchor.
            fixture_rec(
                "pty-exit-30min-early",
                "closed",
                shutdown - 30 * 60_000,
                Some(shutdown - 30 * 60_000),
                Some("pty-exit"),
            ),
            fixture_rec(
                "poll-dead-30min-early",
                "closed",
                shutdown - 31 * 60_000,
                Some(shutdown - 30 * 60_000),
                Some("poll-dead"),
            ),
            // (c) deliberate closes, at the shutdown instant.
            fixture_rec(
                "user-closed",
                "closed",
                shutdown,
                Some(shutdown),
                Some("user-close"),
            ),
            fixture_rec(
                "orphan",
                "closed",
                shutdown,
                Some(shutdown),
                Some("no-terminal"),
            ),
            // (d) finished.
            finished,
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let expected = vec![
        "graceful-a".to_string(),
        "graceful-b".to_string(),
        "poll-dead-recent".to_string(),
    ];
    // Clean boot: the shutdown marker is the anchor.
    assert_eq!(restorable_ids(&store, Some(shutdown), true), expected);
    // Unclean boot (marker excluded): the rows' own stamps anchor at the
    // same instant, so the answer is identical.
    assert_eq!(
        restorable_ids(&store, Some(shutdown - 86_400_000), false),
        expected
    );
}

/// The ledger roster ([`SessionLifecycleStore::roster_records`]) is the
/// restore set with FINISHED rows kept: a graceful-stop cohort (every row
/// closed `pty-exit`) is non-empty and a finished row stays in it (so
/// Finish is undoable from the roster), while a stale `open` ghost, a
/// deliberate close and the losing row of a terminal collision are left
/// out — exactly as the restore set leaves them out, so the "will come
/// back" preview matches the next boot's restore census.
#[test]
fn roster_records_is_the_restore_set_plus_finished_rows() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    let mut finished = fixture_rec(
        "finished-pty-exit",
        "closed",
        shutdown,
        Some(shutdown),
        Some("pty-exit"),
    );
    finished.finished_at = Some(shutdown - 1_000);
    // Two open rows on ONE terminal: only the newer survives the dedupe.
    let mut collided_old = fixture_rec("collided-old", "open", shutdown - 5_000, None, None);
    collided_old.terminal_id = "term-shared".to_string();
    let mut collided_new = fixture_rec("collided-new", "open", shutdown - 1_000, None, None);
    collided_new.terminal_id = "term-shared".to_string();
    write_fixture(
        &path,
        vec![
            fixture_rec(
                "graceful",
                "closed",
                shutdown,
                Some(shutdown),
                Some("pty-exit"),
            ),
            finished,
            fixture_rec("stale-open", "open", shutdown - 72 * 3_600_000, None, None),
            collided_old,
            collided_new,
            fixture_rec(
                "user-closed",
                "closed",
                shutdown,
                Some(shutdown),
                Some("user-close"),
            ),
            fixture_rec(
                "stale-pty-exit",
                "closed",
                shutdown - 3_600_000,
                Some(shutdown - 3_600_000),
                Some("pty-exit"),
            ),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    let mut ids: Vec<String> = store
        .roster_records(Some(shutdown), true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "collided-new".to_string(),
            "finished-pty-exit".to_string(),
            "graceful".to_string(),
        ]
    );
    // …and the restore set is exactly its unfinished part.
    assert_eq!(
        restorable_ids(&store, Some(shutdown), true),
        vec!["collided-new".to_string(), "graceful".to_string()]
    );
}

/// A restore re-asserts the rows it brings back (`record_open` stamps
/// `last_seen_at = now`). That must not move the window the REST of the
/// prior cohort is judged against: rows closed `pty-exit` at the shutdown
/// instant T stay admitted after a sibling was re-opened 30 minutes into
/// this process's life — otherwise a second page restored later in a long
/// rebuild restores nothing and the ledger poll drops that cohort.
#[test]
fn this_process_restoring_a_sibling_does_not_age_out_the_prior_cohort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    let boot = shutdown + 3 * 3_600_000;
    write_fixture(
        &path,
        ["page-a-sib", "page-b-1", "page-b-2"]
            .iter()
            .map(|id| fixture_rec(id, "closed", shutdown, Some(shutdown), Some("pty-exit")))
            .collect(),
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(boot);

    // Page A restores its sibling 30 minutes after boot (any instant after
    // `boot` models it: `record_open` stamps the real clock).
    let mut reopened = fixture_rec("page-a-sib", "open", boot + 30 * 60_000, None, None);
    reopened.terminal_id = "term-new-a".to_string();
    store.record_open(reopened);

    let expected = vec![
        "page-a-sib".to_string(),
        "page-b-1".to_string(),
        "page-b-2".to_string(),
    ];
    assert_eq!(restorable_ids(&store, Some(shutdown), true), expected);
    // The roster (what the ledger poll captures) keeps the cohort too.
    let mut roster: Vec<String> = store
        .roster_records(Some(shutdown), true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    roster.sort();
    assert_eq!(roster, expected);
}

/// A row stamped during THIS process's life is judged against this
/// process's own latest activity, not the prior anchor: a tab closed
/// `pty-exit` an hour ago while the runner kept running is stale.
#[test]
fn a_close_during_this_process_is_judged_against_this_process() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let boot = 1_700_000_000_000_i64;
    write_fixture(
        &path,
        vec![
            fixture_rec(
                "closed-an-hour-ago",
                "closed",
                boot + 3_600_000,
                Some(boot + 3_600_000),
                Some("pty-exit"),
            ),
            fixture_rec("live", "open", boot + 2 * 3_600_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(boot);
    assert_eq!(
        restorable_ids(&store, Some(boot - 60_000), true),
        vec!["live".to_string()]
    );
}

/// Crash boot, SPARSE registry: the only row is a session the operator
/// exited (`pty-exit`) five hours before the crash. The rows alone anchor
/// at that exit and resurrect it; the snapshot history's liveness
/// heartbeat from just before the crash bounds the anchor and excludes it.
#[test]
fn crash_boot_heartbeat_keeps_a_long_exited_session_out() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let exited = 1_700_000_000_000_i64;
    let last_heartbeat = exited + 5 * 3_600_000;
    write_fixture(
        &path,
        vec![fixture_rec(
            "exited-hours-before",
            "closed",
            exited,
            Some(exited),
            Some("pty-exit"),
        )],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    // Without a history the rows are all there is (the documented
    // degraded case) — the exit is its own anchor.
    assert_eq!(
        restorable_ids(&store, None, false),
        vec!["exited-hours-before".to_string()]
    );

    let history_path = dir.path().join("session-snapshots.jsonl");
    std::fs::write(
        &history_path,
        format!(
            "{}\n",
            serde_json::json!({
                "ts": last_heartbeat,
                "at": "2023-11-15T03:13:20Z",
                "reason": "heartbeat",
                "sessions": []
            })
        ),
    )
    .unwrap();
    store.attach_snapshot_history(Arc::new(SnapshotHistory::open(&history_path).unwrap()));
    assert!(restorable_ids(&store, None, false).is_empty());
    // A clean boot is anchored by its marker, never the heartbeat.
    assert_eq!(
        restorable_ids(&store, Some(exited), true),
        vec!["exited-hours-before".to_string()]
    );
}

/// Crash boot after an INTERMEDIATE short-lived boot: an open cohort last
/// seen at T, and an intermediate boot that heartbeated the snapshot
/// history at T+1h46m before dying. The heartbeat bounds only the closed
/// arms, so the open cohort still restores (the 2026-07-19 stranding),
/// while the sparse-registry `pty-exit` row stays excluded.
#[test]
fn crash_boot_intermediate_heartbeat_does_not_strand_an_open_cohort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    let intermediate_heartbeat = t + 3_600_000 + 46 * 60_000;
    let exited = t - 5 * 3_600_000;
    write_fixture(
        &path,
        vec![
            fixture_rec("cohort-1", "open", t, None, None),
            fixture_rec("cohort-2", "open", t - 60_000, None, None),
            fixture_rec("exited", "closed", exited, Some(exited), Some("pty-exit")),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    let history_path = dir.path().join("session-snapshots.jsonl");
    std::fs::write(
        &history_path,
        format!(
            "{}\n",
            serde_json::json!({
                "ts": intermediate_heartbeat,
                "at": "2023-11-14T23:59:20Z",
                "reason": "heartbeat",
                "sessions": []
            })
        ),
    )
    .unwrap();
    store.attach_snapshot_history(Arc::new(SnapshotHistory::open(&history_path).unwrap()));
    assert_eq!(
        restorable_ids(&store, None, false),
        vec!["cohort-1".to_string(), "cohort-2".to_string()],
        "the open cohort is anchored by its own rows, never the heartbeat"
    );
}

/// A `needs-account` row waiting for the operator is held by the poll
/// (never refreshed — the hold itself keeps it on the roster and in the
/// restore set) instead of being closed `no-terminal`, and the wait ends on
/// a verified re-open, a close, or Finish.
#[test]
fn an_awaiting_account_row_is_held_until_resumed_closed_or_finished() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let old = Utc::now().timestamp_millis() - 3_600_000;
    write_fixture(
        &path,
        vec![
            fixture_rec("needs-account", "open", old, None, None),
            fixture_rec("finish-me", "open", old, None, None),
            fixture_rec("close-me", "open", old, None, None),
            fixture_rec("not-waiting", "open", old, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    for id in ["needs-account", "finish-me", "close-me"] {
        store.mark_awaiting_account(id, Utc::now().timestamp_millis(), &NONE_LIVE);
    }
    let now = Utc::now().timestamp_millis();
    assert!(!store.poll_hold_awaiting_account("not-waiting", now));
    assert!(store.poll_hold_awaiting_account("needs-account", now));
    assert_eq!(
        store.get("needs-account").unwrap().last_seen_at,
        old,
        "the hold never refreshes the row's last-seen"
    );

    store
        .set_finished("finish-me", true, None)
        .expect("a known session");
    assert!(!store.poll_hold_awaiting_account("finish-me", now));
    assert_eq!(store.get("finish-me").unwrap().awaiting_account_since, None);
    store.record_close("close-me", "user-close");
    assert!(!store.poll_hold_awaiting_account("close-me", now));
    assert_eq!(store.get("close-me").unwrap().awaiting_account_since, None);
    store.record_open(fixture_rec("needs-account", "open", old, None, None));
    assert!(!store.poll_hold_awaiting_account("needs-account", now));
    assert_eq!(
        store.get("needs-account").unwrap().awaiting_account_since,
        None,
        "a verified resume is new life — the hold clock is cleared"
    );
    // A closed row cannot start waiting.
    store.mark_awaiting_account("close-me", Utc::now().timestamp_millis(), &NONE_LIVE);
    assert!(!store.poll_hold_awaiting_account("close-me", now));
}

/// The hold is capped at the open-row prune age measured from the row's
/// ORIGINAL last-seen; it survives a restart with no re-mark, and a re-mark
/// does not move the cap.
#[test]
fn the_awaiting_account_hold_ends_at_the_prune_age_from_the_original_last_seen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let original = Utc::now().timestamp_millis() - 3_600_000;
    write_fixture(
        &path,
        vec![fixture_rec("waiting", "open", original, None, None)],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("waiting", Utc::now().timestamp_millis(), &NONE_LIVE);
    assert!(store.poll_hold_awaiting_account("waiting", original + OPEN_STALE_MS));
    assert_eq!(
        store.get("waiting").unwrap().last_seen_at,
        original,
        "held, never refreshed"
    );

    // A restart: the durable stamp alone holds the row, and a re-mark
    // keeps the ORIGINAL last-seen.
    drop(store);
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert!(store.poll_hold_awaiting_account("waiting", original + OPEN_STALE_MS));
    store.mark_awaiting_account("waiting", Utc::now().timestamp_millis(), &NONE_LIVE);
    assert_eq!(
        store.get("waiting").unwrap().awaiting_account_since,
        Some(original)
    );
    assert!(
        !store.poll_hold_awaiting_account("waiting", original + OPEN_STALE_MS + 1),
        "past the prune age from the original last-seen, the wait ends"
    );
    assert_eq!(
        store.get("waiting").unwrap().awaiting_account_since,
        None,
        "an expired hold clears its stamp"
    );
    assert!(
        !store.poll_hold_awaiting_account("waiting", original),
        "and stays ended until something marks it again"
    );
}

/// The hold must never move the restore anchor (review MAJOR 1). An
/// intermediate boot B1 (crash at T, boot at T+2h) holds a needs-account
/// row for one poll tick and dies; the crash boot B2 must still restore
/// the un-held open cohort last seen at T, and keep the held row on the
/// roster as needs-account — with no re-mark.
#[test]
fn an_intermediate_boots_hold_never_strands_the_open_cohort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let crash_at = Utc::now().timestamp_millis() - 2 * 3_600_000;
    write_fixture(
        &path,
        vec![
            fixture_rec("cohort-1", "open", crash_at, None, None),
            fixture_rec("cohort-2", "open", crash_at - 60_000, None, None),
            fixture_rec("held", "open", crash_at - 30_000, None, None),
        ],
    );

    // B1: holds the row, one poll tick, dies.
    let b1 = SessionLifecycleStore::open(&path).unwrap();
    b1.mark_awaiting_account("held", Utc::now().timestamp_millis(), &NONE_LIVE);
    let (mut dead, mut no_match) = (HashMap::new(), HashMap::new());
    assert!(poll_holds_unmatched_row(
        &b1,
        "held",
        PollTerminalMatch::Unmatched,
        Utc::now().timestamp_millis(),
        &mut dead,
        &mut no_match
    ));
    drop(b1);

    // B2: a crash boot (no clean marker).
    let b2 = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        restorable_ids(&b2, None, false),
        vec![
            "cohort-1".to_string(),
            "cohort-2".to_string(),
            "held".to_string()
        ],
        "the un-held cohort is anchored at its own crash instant"
    );
    let roster: Vec<String> = b2
        .roster_records(None, false)
        .into_iter()
        .filter(|r| r.awaiting_account_since.is_some())
        .map(|r| r.claude_session_id)
        .collect();
    assert_eq!(roster, vec!["held".to_string()], "still needs-account");
    assert!(
        b2.poll_hold_awaiting_account("held", Utc::now().timestamp_millis()),
        "held across the restart by its durable stamp alone"
    );
}

/// A held row's stamps feed no anchor, even when something else refreshed
/// it: a held row last seen AFTER the cohort cannot strand the cohort.
#[test]
fn a_held_rows_stamp_feeds_no_restore_anchor() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let crash_at = Utc::now().timestamp_millis() - 3 * 3_600_000;
    let mut held = fixture_rec("held", "open", crash_at + 2 * 3_600_000, None, None);
    held.awaiting_account_since = Some(crash_at);
    write_fixture(
        &path,
        vec![fixture_rec("cohort", "open", crash_at, None, None), held],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        restorable_ids(&store, None, false),
        vec!["cohort".to_string(), "held".to_string()]
    );
}

/// The poll's hold branch: an unmatched awaiting row is skipped and its
/// close-detection streaks reset; a matched one, or one not awaiting, is
/// classified as usual (streaks untouched).
#[test]
fn poll_holds_only_an_unmatched_awaiting_row_and_resets_its_streaks() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seen = Utc::now().timestamp_millis();
    write_fixture(
        &path,
        vec![
            fixture_rec("waiting", "open", seen, None, None),
            fixture_rec("plain", "open", seen, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("waiting", Utc::now().timestamp_millis(), &NONE_LIVE);
    let streaks = || -> HashMap<String, u32> {
        [("waiting".to_string(), 2), ("plain".to_string(), 2)]
            .into_iter()
            .collect()
    };
    let (mut dead, mut no_match) = (streaks(), streaks());

    assert!(!poll_holds_unmatched_row(
        &store,
        "waiting",
        PollTerminalMatch::Direct,
        seen,
        &mut dead,
        &mut no_match
    ));
    assert_eq!(
        no_match.get("waiting"),
        Some(&2),
        "a matched row is not held"
    );
    assert!(!poll_holds_unmatched_row(
        &store,
        "plain",
        PollTerminalMatch::Unmatched,
        seen,
        &mut dead,
        &mut no_match
    ));
    assert_eq!(
        no_match.get("plain"),
        Some(&2),
        "a row not awaiting is not held"
    );

    assert!(poll_holds_unmatched_row(
        &store,
        "waiting",
        PollTerminalMatch::Unmatched,
        seen,
        &mut dead,
        &mut no_match
    ));
    assert_eq!(dead.get("waiting"), None);
    assert_eq!(no_match.get("waiting"), None);

    // Past the cap the same branch lets the poll classify it again.
    assert!(!poll_holds_unmatched_row(
        &store,
        "waiting",
        PollTerminalMatch::Unmatched,
        seen + OPEN_STALE_MS + 1,
        &mut dead,
        &mut no_match
    ));
}

/// At boot, every restore candidate the frontend would call
/// `needs-account` is held, so a page opened later still finds it: the
/// classifier's gates first (an unconfirmed, phantom, reconciled,
/// origin-less or transcript-absent row restores terminal-only and is
/// never held), then the account — a recorded dir or a unique transcript
/// holder is not held.
#[test]
fn boot_holds_every_restore_candidate_whose_account_is_unknown() {
    #[derive(Debug)]
    struct Holders;
    impl TranscriptProbe for Holders {
        fn transcript_exists(&self, session_id: &str, _: Option<&str>) -> bool {
            session_id != "no-transcript"
        }
        fn transcript_config_dirs(
            &self,
            session_id: &str,
            _: Option<&str>,
        ) -> Option<Vec<PathBuf>> {
            Some(match session_id {
                "one-holder" => vec![PathBuf::from("/h/.claude-x")],
                "two-holders" => {
                    vec![PathBuf::from("/h/.claude-x"), PathBuf::from("/h/.claude-y")]
                }
                _ => Vec::new(),
            })
        }
    }
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    let unrecorded = |id: &str| {
        let mut r = fixture_rec(id, "open", shutdown, None, None);
        r.config_dir = None;
        r.working_dir = Some("/w".to_string());
        r.origin = Some("authoritative".to_string());
        r.confirmed_at = Some(shutdown - 1_000);
        r
    };
    let mut recorded = unrecorded("recorded");
    recorded.config_dir = Some("/h/.claude-z".to_string());
    let mut observed = unrecorded("observed");
    observed.origin = Some("observed".to_string());
    // Rows the frontend restores terminal-only — never needs-account.
    let mut unconfirmed = unrecorded("unconfirmed");
    unconfirmed.confirmed_at = None;
    let mut phantom = unrecorded("phantom");
    phantom.confirmed_at = None;
    phantom.origin = Some("observed".to_string());
    let mut reconciled = unrecorded("reconciled");
    reconciled.origin = Some("reconciled".to_string());
    let mut no_origin = unrecorded("no-origin");
    no_origin.origin = None;
    let no_transcript = unrecorded("no-transcript");
    // Parity, not a gemini rule: no Gemini adapter ships, so the frontend's
    // `providerDescriptorFor` (like `adapter_for` here) resolves `gemini`
    // to the full-tier Claude descriptor and calls this row needs-account.
    // When a Gemini adapter lands on both sides this row stops being held.
    let mut gemini = unrecorded("gemini");
    gemini.provider = "gemini".to_string();
    write_fixture(
        &path,
        vec![
            recorded,
            observed,
            unconfirmed,
            phantom,
            reconciled,
            no_origin,
            no_transcript,
            gemini,
            unrecorded("one-holder"),
            unrecorded("two-holders"),
            unrecorded("no-holder"),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    let mut held = store.hold_unresolved_account_candidates(Some(shutdown), true, &Holders);
    held.sort();
    assert_eq!(
        held,
        vec![
            "gemini".to_string(),
            "no-holder".to_string(),
            "observed".to_string(),
            "two-holders".to_string()
        ]
    );
    // The poll's first tick after this boot holds them.
    let now = shutdown + 60_000;
    assert!(store.poll_hold_awaiting_account("two-holders", now));
    assert!(store.poll_hold_awaiting_account("no-holder", now));
    assert!(!store.poll_hold_awaiting_account("one-holder", now));
}

/// A real on-screen session whose `pty-exit` close never flushed during
/// a clean shutdown (the webview died before `handleExit` wrote) leaves
/// an `open` row with a fresh-relative-to-anchor `last_seen_at` — it
/// must restore on the clean boot (no silent session loss).
#[test]
fn restorable_records_admit_unflushed_close_open_row_on_clean_boot() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    write_fixture(
        &path,
        vec![
            fixture_rec(
                "flushed",
                "closed",
                shutdown,
                Some(shutdown),
                Some("pty-exit"),
            ),
            // Last touched by the 45s poll shortly before shutdown; its
            // pty-exit close never flushed.
            fixture_rec("unflushed", "open", shutdown - 90_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(
        restorable_ids(&store, Some(shutdown), true),
        vec!["flushed".to_string(), "unflushed".to_string()],
        "an open row within grace of the anchor restores even on a clean boot"
    );
}

/// A registry whose ONLY row is a stale open ghost self-anchors (its own
/// `last_seen_at` is the max) — the prior shutdown marker supplies the
/// honest "last moment of life" and excludes it.
#[test]
fn restorable_records_marker_anchor_excludes_lone_ghost() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    let ghost_seen = shutdown - 72 * 3_600_000;
    write_fixture(
        &path,
        vec![fixture_rec("ghost", "open", ghost_seen, None, None)],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    assert!(
        restorable_ids(&store, Some(shutdown), true).is_empty(),
        "the clean-boot marker anchor must exclude a lone stale ghost"
    );
    // Documented fallback: with no marker and no sibling rows the ghost
    // self-anchors and is admitted (defensive — better than losing a
    // real lone session on a registry with no other signal).
    assert_eq!(
        restorable_ids(&store, None, true),
        vec!["ghost".to_string()]
    );
}

#[test]
fn restorable_records_includes_in_grace_poll_dead() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // A poll-dead close inside the grace window IS restorable — the
    // shell was still alive when the poll fired, so an immediate restart
    // must bring it back rather than silently drop it.
    store.record_open(rec("poll-dead-fresh"));
    store.record_close("poll-dead-fresh", "poll-dead");
    // A poll-dead close aged beyond the grace window is NOT restorable.
    store.record_open(rec("poll-dead-stale"));
    store.record_close("poll-dead-stale", "poll-dead");

    let now = Utc::now().timestamp_millis();

    // Age `poll-dead-fresh` to now-30s (well inside grace) and
    // `poll-dead-stale` past the grace window.
    age_persisted_records(&store, &path, |m| {
        m.get_mut("poll-dead-fresh").unwrap().closed_at = Some(now - 30_000);
        m.get_mut("poll-dead-stale").unwrap().closed_at =
            Some(now - RESTORABLE_POLL_DEAD_MS - 1000);
    });
    let store = SessionLifecycleStore::open(&path).unwrap();

    let ids: Vec<String> = store
        .restorable_records(None, true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    assert!(
        ids.contains(&"poll-dead-fresh".to_string()),
        "poll-dead closed 30s ago must be restorable; got {ids:?}"
    );
    assert!(
        !ids.contains(&"poll-dead-stale".to_string()),
        "poll-dead aged past grace must NOT be restorable; got {ids:?}"
    );
}

/// The 2026-07-19 mass-strand, reproduced. A crash cohort dies together at
/// ~T; the runner is restarted ~2h later; an INTERMEDIATE boot during the
/// downtime rewrote the shutdown marker to a later instant (T+90m). Under
/// the old anchor that later marker `at` fed the global max and evicted
/// every crash row (`anchor - last_seen = 90m > 10m grace`) — 81 sessions
/// lost. Gating the marker to CLEAN boots only (this boot is unclean) keeps
/// the whole cohort restorable: the rows supply the anchor themselves, at ~T.
#[test]
fn restorable_records_crash_cohort_survives_delayed_restart_marker_gated() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64; // crash instant
    let ninety_min = 90 * 60_000_i64;
    // restarted 2h later — irrelevant: admission is anchor-relative.

    // Five open rows that stopped together at ~T (the crash cohort).
    write_fixture(
        &path,
        vec![
            fixture_rec("crash-0", "open", t, None, None),
            fixture_rec("crash-1", "open", t - 1_000, None, None),
            fixture_rec("crash-2", "open", t - 2_000, None, None),
            fixture_rec("crash-3", "open", t - 3_000, None, None),
            fixture_rec("crash-4", "open", t - 4_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    // Unclean (crash) boot: the intermediate marker at T+90m is EXCLUDED
    // from the anchor, so the crash rows anchor themselves and all survive.
    let marker_at = Some(t + ninety_min);
    let ids = restorable_ids(&store, marker_at, false);
    assert_eq!(
        ids,
        vec![
            "crash-0".to_string(),
            "crash-1".to_string(),
            "crash-2".to_string(),
            "crash-3".to_string(),
            "crash-4".to_string(),
        ],
        "the whole crash cohort survives a delayed restart (got {ids:?})"
    );

    // Control: on a CLEAN boot the same later marker IS an honest
    // last-moment-of-life signal and correctly evicts the now-stale cohort.
    assert!(
        restorable_ids(&store, marker_at, true).is_empty(),
        "a clean-boot marker 90m newer than the cohort excludes it"
    );
}

/// A genuine stale lone ghost (its terminal died 72h before the crash) stays
/// EXCLUDED: a fresh crash band supplies the anchor and the ghost is far
/// outside grace below it.
#[test]
fn restorable_records_still_excludes_stale_ghost() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    // 1h later — irrelevant: admission is anchor-relative.

    write_fixture(
        &path,
        vec![
            fixture_rec("crash-0", "open", t, None, None),
            fixture_rec("crash-1", "open", t - 1_000, None, None),
            fixture_rec("crash-2", "open", t - 2_000, None, None),
            fixture_rec("stale-ghost", "open", t - 72 * 3_600_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let ids = restorable_ids(&store, None, false);
    assert_eq!(
        ids,
        vec![
            "crash-0".to_string(),
            "crash-1".to_string(),
            "crash-2".to_string(),
        ],
        "the crash band restores; the 72h stale ghost stays excluded"
    );
}

/// The incident's measured shape (plan
/// `2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-restore-set`):
/// the liveness poll's process snapshots failed for the last stretch of
/// the prior process's life, so no row was TOUCHED after T, while sessions
/// kept being spawned up to T+39m. The clean marker was stamped at T+77m.
/// Boot at T+3h.
const STARVED_COHORT: [&str; 5] = [
    "starved-0000",
    "starved-0010",
    "starved-0020",
    "starved-0030",
    "starved-0039",
];

/// Five open rows spread from `t` to `t + 39m` (ids name their minute
/// offset) plus one ghost last seen 72h before `t`.
fn starved_cohort_fixture(t: i64) -> Vec<TerminalSessionRecord> {
    let minute = 60_000_i64;
    let mut recs: Vec<TerminalSessionRecord> = [0_i64, 10, 20, 30, 39]
        .iter()
        .zip(STARVED_COHORT)
        .map(|(m, id)| fixture_rec(id, "open", t + m * minute, None, None))
        .collect();
    recs.push(fixture_rec(
        "ghost-72h",
        "open",
        t - 72 * 3_600_000,
        None,
        None,
    ));
    recs
}

/// Attach an observation epoch to `store` as THIS process would at boot:
/// when `prior_observed_at` is set, a PRIOR process first records it into
/// the sidecar under `dir`.
fn attach_epoch(
    store: &SessionLifecycleStore,
    dir: &Path,
    prior_observed_at: Option<i64>,
) -> Arc<ObservationEpoch> {
    let path = dir.join(crate::session::observation_epoch::EPOCH_FILE_NAME);
    if let Some(at) = prior_observed_at {
        assert!(ObservationEpoch::open(&path, at - 3_600_000).record_tick(true, at, &[]));
    }
    let epoch = Arc::new(ObservationEpoch::open(&path, i64::MAX - 1));
    store.attach_observation_epoch(epoch.clone());
    epoch
}

/// The prior process's last SUCCESSFUL observation was at T (no touch
/// landed after it), so the cohort is fresh relative to what the runner
/// could see; the 72h ghost — stale long before T — stays out.
#[test]
fn restorable_records_snapshot_starved_cohort_survives_early_clean_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    let marker_at = t + 77 * 60_000;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    attach_epoch(&store, dir.path(), Some(t));

    assert_eq!(
        restorable_ids(&store, Some(marker_at), true),
        STARVED_COHORT.map(str::to_string).to_vec(),
        "every session on screen when the snapshots starved is restored; the 72h ghost is not"
    );
}

/// The rows of [`starved_cohort_fixture`] within the open-row grace of the
/// cohort's own newest stamp (`t + 39m`): what self-anchoring admits.
const STARVED_COHORT_NEWEST_BAND: [&str; 2] = ["starved-0030", "starved-0039"];

/// The documented one-boot transition, as production runs it: NO epoch
/// (first boot after the upgrade, or a pre-fix prior process), and the
/// old marker carries no `teardown_complete` field, so it reads `false`
/// and boot attaches `false`. The open anchor is then the prior process's
/// row stamps alone: the rows within the grace of the cohort's newest
/// stamp restore and the older ones still strand. Pinned so the fallback
/// is a decision, not an accident.
#[test]
fn restorable_records_snapshot_starved_cohort_without_epoch_still_strands() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    store.attach_prior_teardown_complete(false);
    let epoch = attach_epoch(&store, dir.path(), None);
    assert!(
        epoch.boot_notice().is_some(),
        "the absent epoch is announced"
    );

    assert_eq!(
        restorable_ids(&store, Some(t + 77 * 60_000), true),
        STARVED_COHORT_NEWEST_BAND.map(str::to_string).to_vec()
    );
}

/// With no teardown evidence attached at all (the store used standalone),
/// the marker anchors open rows as it always did, so with no epoch the
/// whole starved cohort strands.
#[test]
fn restorable_records_without_epoch_or_teardown_evidence_keeps_the_marker() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    attach_epoch(&store, dir.path(), None);

    assert!(restorable_ids(&store, Some(t + 77 * 60_000), true).is_empty());
}

/// A corrupt sidecar reads as absent — the same fallback as no epoch, in
/// the production shape (the pre-field marker attaches `false`).
#[test]
fn restorable_records_corrupt_epoch_falls_back_to_prior() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    store.attach_prior_teardown_complete(false);
    std::fs::write(
        dir.path()
            .join(crate::session::observation_epoch::EPOCH_FILE_NAME),
        b"{\"observedOkAt\": \"not a number\"}",
    )
    .unwrap();
    attach_epoch(&store, dir.path(), None);

    assert_eq!(
        restorable_ids(&store, Some(t + 77 * 60_000), true),
        STARVED_COHORT_NEWEST_BAND.map(str::to_string).to_vec()
    );
}

/// An epoch OLDER than every row (the poll last saw the table an hour
/// before the cohort's earliest stamp) admits the whole cohort; the 72h
/// ghost is still 71h behind it.
#[test]
fn restorable_records_epoch_older_than_every_row_admits_the_cohort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    attach_epoch(&store, dir.path(), Some(t - 3_600_000));

    assert_eq!(
        restorable_ids(&store, Some(t + 77 * 60_000), true),
        STARVED_COHORT.map(str::to_string).to_vec()
    );
}

/// The grace boundary, both sides, measured from an epoch NEWER than the
/// rows: 9:59 behind it is admitted, 10:01 behind it is not.
#[test]
fn restorable_records_epoch_newer_than_rows_keeps_the_grace_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let epoch_at = 1_700_000_000_000_i64;
    write_fixture(
        &path,
        vec![
            fixture_rec("behind-0959", "open", epoch_at - 599_000, None, None),
            fixture_rec("behind-1001", "open", epoch_at - 601_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(epoch_at + 3 * 3_600_000);
    attach_epoch(&store, dir.path(), Some(epoch_at));

    assert_eq!(
        restorable_ids(&store, Some(epoch_at + 3_600_000), true),
        vec!["behind-0959".to_string()]
    );
}

/// A CRASH boot (no clean marker counts) with an epoch: the rows alone
/// anchor at T+39m and strand the early cohort; the epoch at T admits it.
#[test]
fn restorable_records_crash_boot_with_epoch_admits_the_starved_cohort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    // Control: without the epoch the crash cohort anchors at its newest
    // spawn and loses every row more than 10 minutes older.
    assert_eq!(
        restorable_ids(&store, Some(t - 3_600_000), false),
        vec!["starved-0030".to_string(), "starved-0039".to_string()]
    );
    attach_epoch(&store, dir.path(), Some(t));
    assert_eq!(
        restorable_ids(&store, Some(t - 3_600_000), false),
        STARVED_COHORT.map(str::to_string).to_vec()
    );
}

/// An epoch THIS process writes never feeds the prior arm: its own ticks
/// (or a restore's re-asserts) cannot move the window the prior cohort
/// is judged against — in either direction.
#[test]
fn restorable_records_this_process_epoch_does_not_feed_prior() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    let boot = t + 3 * 3_600_000;
    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(boot);
    // No prior epoch: this process observing early (t) must not rescue
    // the prior cohort, and observing late must not age it further.
    let epoch = attach_epoch(&store, dir.path(), None);
    epoch.record_ok(t);
    assert!(restorable_ids(&store, Some(t + 77 * 60_000), true).is_empty());

    let dir2 = tempdir().unwrap();
    let path2 = dir2.path().join("terminal-sessions.json");
    write_fixture(&path2, starved_cohort_fixture(t));
    let store2 = SessionLifecycleStore::open(&path2).unwrap();
    store2.set_boot_ms_for_test(boot);
    let epoch2 = attach_epoch(&store2, dir2.path(), Some(t));
    epoch2.record_ok(boot + 5 * 3_600_000);
    assert_eq!(
        restorable_ids(&store2, Some(t + 77 * 60_000), true),
        STARVED_COHORT.map(str::to_string).to_vec()
    );
}

/// A second boot reads the epoch the first boot's poll wrote.
#[test]
fn restorable_records_second_boot_reads_the_first_boots_epoch() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    let epoch_path = dir
        .path()
        .join(crate::session::observation_epoch::EPOCH_FILE_NAME);
    // First boot: a failed tick, then a successful one at T, then failures.
    let first = ObservationEpoch::open(&epoch_path, t - 3_600_000);
    assert!(!first.record_tick(false, t - 45_000, &[]));
    assert!(first.record_tick(true, t, &[]));
    assert!(!first.record_tick(false, t + 45_000, &[]));
    drop(first);

    write_fixture(&path, starved_cohort_fixture(t));
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t + 3 * 3_600_000);
    let second = Arc::new(ObservationEpoch::open(&epoch_path, t + 3 * 3_600_000));
    assert_eq!(second.prior_observed(), Some(t));
    store.attach_observation_epoch(second);
    assert_eq!(
        restorable_ids(&store, Some(t + 77 * 60_000), true),
        STARVED_COHORT.map(str::to_string).to_vec()
    );
}

/// The ghost guard rails hold with an epoch set AFTER the ghost went
/// stale: successful ticks kept not finding it alive.
#[test]
fn ghost_exclusions_hold_with_an_epoch_after_the_ghost() {
    let shutdown = 1_700_000_000_000_i64;
    // `restorable_records_marker_anchor_excludes_lone_ghost` shape.
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    write_fixture(
        &path,
        vec![fixture_rec(
            "ghost",
            "open",
            shutdown - 72 * 3_600_000,
            None,
            None,
        )],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    attach_epoch(&store, dir.path(), Some(shutdown - 60_000));
    assert!(restorable_ids(&store, Some(shutdown), true).is_empty());

    // `restorable_records_exclude_stale_open_ghost_after_clean_restart` shape.
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    write_fixture(
        &path,
        vec![
            fixture_rec(
                "onscreen",
                "closed",
                shutdown,
                Some(shutdown),
                Some("pty-exit"),
            ),
            fixture_rec("ghost", "open", shutdown - 72 * 3_600_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    attach_epoch(&store, dir.path(), Some(shutdown - 60_000));
    assert_eq!(
        restorable_ids(&store, Some(shutdown), true),
        vec!["onscreen".to_string()]
    );
}

/// The live arm still ages out a row this process OBSERVED dead: with a
/// successful observation 30 minutes after a row's last touch, that row
/// is stale — the epoch only stops UNOBSERVED stretches counting.
#[test]
fn live_arm_still_ages_out_a_row_this_process_observed_dead() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    write_fixture(
        &path,
        vec![
            fixture_rec("touched-at-t", "open", t, None, None),
            fixture_rec("touched-at-t30", "open", t + 30 * 60_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(t - 60_000);
    attach_epoch(&store, dir.path(), None).record_ok(t + 30 * 60_000);
    let roster: Vec<String> = store
        .roster_records(Some(t - 120_000), true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    assert_eq!(roster, vec!["touched-at-t30".to_string()]);
}

// Phase 3: the marker anchors open rows only after a FINISHED teardown.

const SELF_ANCHORED_COHORT: [&str; 3] = ["cohort-00", "cohort-05", "cohort-09"];

/// Three open rows within 10 minutes of each other, so they self-anchor:
/// only a marker can exclude them.
fn self_anchored_cohort(t: i64) -> Vec<TerminalSessionRecord> {
    SELF_ANCHORED_COHORT
        .iter()
        .zip([0_i64, 5, 9])
        .map(|(id, m)| fixture_rec(id, "open", t + m * 60_000, None, None))
        .collect()
}

/// A store over `recs`, booted at `boot`, carrying the prior teardown bit.
fn store_with_teardown(
    dir: &Path,
    recs: Vec<TerminalSessionRecord>,
    boot: i64,
    teardown_complete: bool,
) -> SessionLifecycleStore {
    let path = dir.join("terminal-sessions.json");
    write_fixture(&path, recs);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(boot);
    store.attach_prior_teardown_complete(teardown_complete);
    store
}

/// No epoch: a clean marker 77 minutes past a self-anchored cohort does
/// not exclude it when its teardown was cut short. The finished-teardown
/// marker in the same place excludes all three, as it always did.
#[test]
fn restorable_records_incomplete_teardown_marker_does_not_exclude_the_cohort() {
    let t = 1_700_000_000_000_i64;
    let marker = Some(t + 77 * 60_000);
    let cohort = SELF_ANCHORED_COHORT.map(str::to_string).to_vec();
    for (complete, expected) in [(false, cohort), (true, Vec::new())] {
        let dir = tempdir().unwrap();
        let store = store_with_teardown(
            dir.path(),
            self_anchored_cohort(t),
            t + 3 * 3_600_000,
            complete,
        );
        assert_eq!(
            restorable_ids(&store, marker, true),
            expected,
            "teardown_complete={complete}"
        );
    }
}

/// (i) With an observation epoch the teardown bit changes nothing: the
/// epoch caps the open anchor either way. The lone ghost the poll observed
/// dead is the shape that tells them apart if the gate ignored the epoch
/// (the incomplete marker would leave it to self-anchor), so it is pinned
/// beside the starved cohort.
#[test]
fn restorable_records_with_an_epoch_the_teardown_bit_changes_nothing() {
    let t = 1_700_000_000_000_i64;
    let shapes = [
        (
            "starved cohort",
            starved_cohort_fixture(t),
            t,
            t + 77 * 60_000,
            STARVED_COHORT.map(str::to_string).to_vec(),
        ),
        (
            "lone ghost",
            vec![fixture_rec("ghost", "open", t - 72 * 3_600_000, None, None)],
            t - 60_000,
            t,
            Vec::new(),
        ),
    ];
    for (shape, recs, epoch_at, marker_at, expected) in shapes {
        for complete in [true, false] {
            let dir = tempdir().unwrap();
            let store = store_with_teardown(dir.path(), recs.clone(), t + 3 * 3_600_000, complete);
            attach_epoch(&store, dir.path(), Some(epoch_at));
            assert_eq!(
                restorable_ids(&store, Some(marker_at), true),
                expected,
                "{shape}, teardown_complete={complete}"
            );
        }
    }
}

/// (ii) No epoch and an interrupted teardown: a lone ghost self-anchors
/// and RESTORES. Asserted so the trade is a decision, not an accident: a
/// ghost pane can be closed, a dropped session cannot be rebuilt. A
/// finished teardown's marker still excludes it (the
/// `restorable_records_marker_anchor_excludes_lone_ghost` shape).
#[test]
fn restorable_records_lone_ghost_restores_only_after_an_interrupted_teardown() {
    let shutdown = 1_700_000_000_000_i64;
    let ghost = || {
        vec![fixture_rec(
            "ghost",
            "open",
            shutdown - 72 * 3_600_000,
            None,
            None,
        )]
    };
    for (complete, expected) in [(false, vec!["ghost".to_string()]), (true, Vec::new())] {
        let dir = tempdir().unwrap();
        let store = store_with_teardown(dir.path(), ghost(), shutdown + 3_600_000, complete);
        assert_eq!(
            restorable_ids(&store, Some(shutdown), true),
            expected,
            "teardown_complete={complete}"
        );
    }
}

/// The closed arms keep the marker whatever its teardown bit: a session
/// exited `pty-exit` an hour before an interrupted teardown is still an
/// old exit, not a restore candidate.
#[test]
fn restorable_records_interrupted_teardown_keeps_the_marker_for_closed_rows() {
    let shutdown = 1_700_000_000_000_i64;
    let exited = shutdown - 3_600_000;
    let dir = tempdir().unwrap();
    let store = store_with_teardown(
        dir.path(),
        vec![fixture_rec(
            "old-exit",
            "closed",
            exited,
            Some(exited),
            Some("pty-exit"),
        )],
        shutdown + 3_600_000,
        false,
    );
    assert!(restorable_ids(&store, Some(shutdown), true).is_empty());
}

/// The drain's own marker stamp, end to end through the boot classifier
/// into admission: a drain stamps `clean` but never a finished teardown,
/// so a self-anchored cohort its marker sits 77 minutes past is still
/// admitted on the next boot.
#[test]
fn restorable_records_admit_the_cohort_behind_a_drain_only_marker() {
    let dir = tempdir().unwrap();
    let marker_path = dir.path().join("last-shutdown.json");
    crate::drain::stamp_drained_marker(&marker_path);
    let boot = crate::session::shutdown_marker::read_classification(&marker_path, i64::MAX);
    let marker_at = boot.prior_marker_at.expect("the drain wrote a marker");
    let store = store_with_teardown(
        dir.path(),
        self_anchored_cohort(marker_at - 77 * 60_000),
        marker_at + 3_600_000,
        boot.teardown_complete,
    );
    assert_eq!(
        restorable_ids(&store, boot.prior_marker_at, !boot.crash_recovery),
        SELF_ANCHORED_COHORT.map(str::to_string).to_vec()
    );
}

/// The in-process arm of the same strand: THIS process booted before the
/// cohort was spawned, its snapshots then failed, so nothing it wrote was
/// ever touched. The ledger roster (what the poll captures every tick) must
/// still hold every row, not only those within 10 minutes of the newest
/// spawn.
#[test]
fn live_roster_keeps_a_starved_cohort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    let boot = t - 60_000;
    let recs: Vec<TerminalSessionRecord> = starved_cohort_fixture(t)
        .into_iter()
        .filter(|r| r.claude_session_id != "ghost-72h")
        .collect();
    write_fixture(&path, recs);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(boot);

    let mut roster: Vec<String> = store
        .roster_records(Some(boot - 60_000), true)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    roster.sort();
    assert_eq!(roster, STARVED_COHORT.map(str::to_string).to_vec());
}

/// A genuinely-newer band advances the anchor and a crash band more than
/// `grace` OLDER is EXCLUDED from the (full-restore) set — it is stale
/// relative to the last moment of life, exactly as an old lone ghost is.
/// (Its row stays in the registry, so it remains resumable by hand from
/// Past Sessions; boot restore does not re-offer it.) This pins the fix for the densest-cohort
/// over-admission regression: an older-but-larger band must NOT re-admit
/// stale sessions by pinning the anchor into the past.
#[test]
fn restorable_records_newer_band_excludes_older_crash_band() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;
    let thirty_min = 30 * 60_000_i64;

    write_fixture(
        &path,
        vec![
            // A LARGER but OLDER band (5 rows) at ~T.
            fixture_rec("old-0", "open", t, None, None),
            fixture_rec("old-1", "open", t - 1_000, None, None),
            fixture_rec("old-2", "open", t - 2_000, None, None),
            fixture_rec("old-3", "open", t - 3_000, None, None),
            fixture_rec("old-4", "open", t - 4_000, None, None),
            // A SMALLER but genuinely-NEWER band (2 rows) 30m later.
            fixture_rec("newer-0", "open", t + thirty_min, None, None),
            fixture_rec("newer-1", "open", t + thirty_min - 1_000, None, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let ids = restorable_ids(&store, None, false);
    assert_eq!(
        ids,
        vec!["newer-0".to_string(), "newer-1".to_string()],
        "only the newest band restores; the 30m-older band is stale (got {ids:?})"
    );
}

/// The read-time one-live-session-per-terminal dedupe: when several admitted
/// OPEN rows share a `terminal_id`, `restorable_records` returns exactly the
/// most-authoritative one (CONFIRMED over unconfirmed, else newest), so the
/// restore read never collapses N rows onto one terminal — regardless of
/// whether the persistent boot repair has run yet.
#[test]
fn restorable_records_dedupes_collided_open_rows_prefers_confirmed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let t = 1_700_000_000_000_i64;

    let mk = |id: &str, terminal: &str, last_seen: i64, confirmed: Option<i64>| {
        let mut r = fixture_rec(id, "open", last_seen, None, None);
        r.terminal_id = terminal.to_string();
        r.confirmed_at = confirmed;
        r
    };
    write_fixture(
        &path,
        vec![
            // Same terminal: an older CONFIRMED real session and a newer
            // UNCONFIRMED phantom (a later re-assert). Confirmed must win.
            mk("confirmed-real", "shared", t - 5_000, Some(t - 5_000)),
            mk("unconfirmed-newer", "shared", t, None),
            // A distinct terminal — always kept.
            mk("solo", "other", t, None),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let mut ids = restorable_ids(&store, None, false);
    ids.sort();
    assert_eq!(
        ids,
        vec!["confirmed-real".to_string(), "solo".to_string()],
        "the confirmed row wins the shared terminal; the phantom is dropped; solo kept"
    );
}

/// Unit coverage for the read-time dedupe helper: confirmed beats a newer
/// unconfirmed on the same terminal; distinct terminals both survive; an
/// empty terminal_id is uncorrelatable and passes through; closed rows pass.
#[test]
fn dedupe_open_by_terminal_prefers_confirmed_then_newest() {
    let mk = |id: &str, state: &str, terminal: &str, last_seen: i64, confirmed: Option<i64>| {
        let mut r = fixture_rec(id, state, last_seen, None, None);
        r.terminal_id = terminal.to_string();
        r.confirmed_at = confirmed;
        r
    };
    let out = dedupe_open_by_terminal(vec![
        mk("conf-old", "open", "A", 1_000, Some(1_000)),
        mk("unconf-new", "open", "A", 2_000, None),
        mk("b-newer", "open", "B", 3_000, None),
        mk("b-older", "open", "B", 1_500, None),
        mk("empty-term-1", "open", "", 4_000, None),
        mk("empty-term-2", "open", "", 5_000, None),
    ]);
    let mut kept: Vec<String> = out.into_iter().map(|r| r.claude_session_id).collect();
    kept.sort();
    assert_eq!(
        kept,
        vec![
            "b-newer".to_string(),      // newest on terminal B
            "conf-old".to_string(),     // confirmed beats newer unconfirmed on A
            "empty-term-1".to_string(), // empty terminal_id: uncorrelatable, both pass
            "empty-term-2".to_string(),
        ]
    );
}

/// `rebind_terminal` re-points an open record at the terminal that now
/// hosts it WITHOUT refreshing `last_seen_at` — the whole reason it exists
/// rather than a `record_open` re-assert. A stale row must keep aging out
/// on schedule even after a cold restore rebinds it, or ghost rows become
/// immortal (the hazard that gated the re-assert to verified resumes and
/// so let the orphan-PTY leak run unbounded).
#[test]
fn rebind_terminal_updates_binding_without_refreshing_liveness() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.record_open(rec("s1"));
    let before = store.get("s1").unwrap();
    assert_eq!(before.terminal_id, "term-abc");

    // Age `last_seen_at` so a refresh would be unmistakable.
    let aged = before.last_seen_at - 600_000;
    age_persisted_records(&store, &path, |m| {
        m.get_mut("s1").unwrap().last_seen_at = aged;
    });
    let store = SessionLifecycleStore::open(&path).unwrap();

    store.rebind_terminal("s1", "term-fresh", 5);

    let after = store.get("s1").unwrap();
    assert_eq!(after.terminal_id, "term-fresh", "binding must move");
    assert_eq!(after.zone_index, 5, "zone must move with the binding");
    assert_eq!(
        after.last_seen_at, aged,
        "rebind must NOT refresh last_seen_at (ghost rows would become immortal)"
    );
    assert_eq!(after.state, "open");
    assert_eq!(
        after.confirmed_at, before.confirmed_at,
        "rebind must not touch provenance"
    );

    // Survives a reload — the rebind is persisted, not in-memory only.
    let reloaded = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(reloaded.get("s1").unwrap().terminal_id, "term-fresh");
}

/// A rebind is a no-op on a record that is absent or no longer `open` —
/// a closed row is not a restore target and must not be silently revived.
#[test]
fn rebind_terminal_is_a_noop_for_absent_or_closed_records() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();

    // Absent — must not panic or create a row.
    store.rebind_terminal("ghost", "term-fresh", 1);
    assert!(store.get("ghost").is_none());

    store.record_open(rec("s2"));
    store.record_close("s2", "user-closed");
    store.rebind_terminal("s2", "term-fresh", 1);

    let after = store.get("s2").unwrap();
    assert_eq!(
        after.terminal_id, "term-abc",
        "a closed record must not be rebound"
    );
    assert_eq!(after.state, "closed");
}

/// Review fixes 4, item 1: an EXPIRED hold stamp is not a hold, so its open
/// row feeds the restore anchor again; a live hold still feeds none.
#[test]
fn an_expired_hold_stamp_feeds_the_restore_anchor_again() {
    let now = Utc::now().timestamp_millis();
    let mut cohort = fixture_rec("cohort", "open", now - 3 * 3_600_000, None, None);
    cohort.awaiting_account_since = None;
    let mut expired = fixture_rec("expired", "open", now - 3_600_000, None, None);
    expired.awaiting_account_since = Some(now - OPEN_STALE_MS - 60_000);
    let mut held = fixture_rec("held", "open", now - 60_000, None, None);
    held.awaiting_account_since = Some(now - 86_400_000);
    let rows = [cohort, expired, held];
    let anchors = restore_anchors(
        rows.iter(),
        None,
        false,
        None,
        None,
        Observed::default(),
        i64::MAX,
        now,
    );
    assert_eq!(
        anchors.prior,
        Some(now - 3_600_000),
        "the expired-stamp row feeds the anchor; the live hold does not"
    );
}

/// Review fixes 4, item 1: a re-mark after the hold expired re-stamps from
/// the row's current last-seen and starts a new hold.
#[test]
fn a_re_mark_after_the_hold_expired_re_stamps() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let last_seen = now - 3_600_000;
    let mut row = fixture_rec("waiting", "open", last_seen, None, None);
    row.awaiting_account_since = Some(now - OPEN_STALE_MS - 60_000);
    write_fixture(&path, vec![row]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("waiting", now, &NONE_LIVE);
    assert_eq!(
        store.get("waiting").unwrap().awaiting_account_since,
        Some(last_seen),
        "an expired stamp is treated as absent"
    );
    assert!(store.poll_hold_awaiting_account("waiting", now));
}

/// Review fixes 4, item 2: a profile resume's restore-pending mark sets the
/// pending marker only — it is not a boot restore, so the census never
/// counts it.
#[test]
fn a_profile_resume_pending_mark_is_not_a_boot_restore() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("profile"));
    store.mark_restore_pending("profile", false);
    let got = store.get("profile").unwrap();
    assert!(got.restore_pending_at.is_some(), "pending is still marked");
    assert_eq!(got.restored_from_boot_at, None);
    assert_eq!(got.restore_tier, None);
    assert!(
        crate::session::restore_census::observe_restored(&store, 0).is_empty(),
        "a profile resume is not in the boot-restore census"
    );
}

/// Review fixes 4, item 2: the profile path never marks a closed row.
#[test]
fn a_profile_resume_pending_mark_skips_a_closed_row() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.record_open(rec("gone"));
    store.record_close("gone", "user-close");
    store.mark_restore_pending("gone", false);
    let got = store.get("gone").unwrap();
    assert_eq!(got.restore_pending_at, None);
    assert_eq!(got.restore_tier, None);
}

/// Review fixes 4, item 3: every close path clears the awaiting-account
/// stamp — the two `record_open` supersede arms and the boot collision
/// repair, not only `record_close`.
///
/// The rows carry an EXPIRED stamp: a live hold detaches its terminal, so a
/// live-held row is never superseded at all (see
/// `a_held_row_is_out_of_terminal_contention_in_the_repair`). An expired
/// stamp is no hold, so its row contends like any other — and its close
/// must not leave the stale stamp behind.
#[test]
fn a_superseded_close_clears_the_awaiting_account_stamp() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let on = |id: &str, terminal: &str, seen: i64, confirmed: bool| {
        let mut r = fixture_rec(id, "open", seen, None, None);
        r.terminal_id = terminal.to_string();
        r.confirmed_at = confirmed.then_some(seen);
        r.awaiting_account_since = Some(now - OPEN_STALE_MS - 60_000);
        r
    };
    write_fixture(
        &path,
        vec![
            on("phantom", "term-a", now - 60_000, false),
            on("prior-run", "term-b", now - 60_000, true),
            on("repair-loser", "term-c", now - 120_000, true),
            on("repair-winner", "term-c", now - 60_000, true),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();

    let mut incoming = rec("new-a");
    incoming.terminal_id = "term-a".to_string();
    incoming.origin = Some("authoritative".to_string());
    store.record_open(incoming);
    let mut incoming = rec("new-b");
    incoming.terminal_id = "term-b".to_string();
    incoming.confirmed_at = Some(now);
    store.record_open(incoming);
    assert_eq!(store.repair_terminal_id_collisions(), 1);

    for (id, reason) in [
        ("phantom", "superseded"),
        ("prior-run", "superseded-terminal-reuse"),
        ("repair-loser", "superseded-terminal-reuse"),
    ] {
        let got = store.get(id).unwrap();
        assert_eq!(got.state, "closed", "{id}");
        assert_eq!(got.close_reason.as_deref(), Some(reason), "{id}");
        assert_eq!(got.awaiting_account_since, None, "{id}: stamp cleared");
    }
}

/// Review fixes 4, item 4: only a DIRECT terminal match releases a held
/// row; a match made by the (page_id, title, working_dir) fallback alone
/// keeps it held.
#[test]
fn a_fallback_only_terminal_match_never_releases_a_held_row() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seen = Utc::now().timestamp_millis();
    write_fixture(&path, vec![fixture_rec("held", "open", seen, None, None)]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("held", seen, &NONE_LIVE);
    let (mut dead, mut no_match) = (HashMap::new(), HashMap::new());
    assert!(
        poll_holds_unmatched_row(
            &store,
            "held",
            PollTerminalMatch::Fallback,
            seen,
            &mut dead,
            &mut no_match
        ),
        "a fallback match is not the row's own terminal"
    );
    assert!(!poll_holds_unmatched_row(
        &store,
        "held",
        PollTerminalMatch::Direct,
        seen,
        &mut dead,
        &mut no_match
    ));
}

/// Review fixes 4, item 5: a held row never wins a terminal from a row
/// admitted on recency — the fresh row's terminal is restored, and the
/// held row is still in the set, still needs-account.
#[test]
fn a_held_row_never_wins_a_terminal_from_a_row_admitted_on_recency() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let crash_at = Utc::now().timestamp_millis() - 3_600_000;
    let mut held = fixture_rec("held", "open", crash_at, None, None);
    held.terminal_id = "term-shared".to_string();
    held.confirmed_at = Some(crash_at - 60_000);
    held.awaiting_account_since = Some(crash_at);
    let mut fresh = fixture_rec("fresh", "open", crash_at - 30_000, None, None);
    fresh.terminal_id = "term-shared".to_string();
    write_fixture(&path, vec![held, fresh]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let restored = store.restorable_records(None, false);
    let on_terminal: Vec<&str> = restored
        .iter()
        .filter(|r| r.terminal_id == "term-shared" && r.awaiting_account_since.is_none())
        .map(|r| r.claude_session_id.as_str())
        .collect();
    assert_eq!(
        on_terminal,
        vec!["fresh"],
        "the fresh row restores the terminal"
    );
    assert!(
        restored
            .iter()
            .any(|r| r.claude_session_id == "held" && r.awaiting_account_since.is_some()),
        "the held row is still offered as needs-account"
    );
}
/// Review fixes 5: the fix-4 fixture — a confirmed row held for an account
/// choice and a fresh unconfirmed row that now owns the held row's old
/// terminal id. The hold detaches the held row, so the boot collision
/// repair finds no collision: the fresh row stays open and the held row
/// stays held.
#[test]
fn a_held_row_is_out_of_terminal_contention_in_the_repair() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let crash_at = Utc::now().timestamp_millis() - 3_600_000;
    let mut held = fixture_rec("held", "open", crash_at, None, None);
    held.terminal_id = "term-shared".to_string();
    held.confirmed_at = Some(crash_at - 60_000);
    let mut fresh = fixture_rec("fresh", "open", crash_at - 30_000, None, None);
    fresh.terminal_id = "term-shared".to_string();
    write_fixture(&path, vec![held, fresh]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let now = Utc::now().timestamp_millis();
    store.mark_awaiting_account("held", now, &NONE_LIVE);

    assert_eq!(
        store.repair_terminal_id_collisions(),
        0,
        "no collision left"
    );
    let fresh = store.get("fresh").unwrap();
    assert_eq!(fresh.state, "open", "the fresh row keeps its terminal");
    assert_eq!(fresh.terminal_id, "term-shared");
    let held = store.get("held").unwrap();
    assert_eq!(held.state, "open");
    assert!(
        awaiting_account_hold(&held, now),
        "the held row is still held"
    );
    assert_eq!(held.terminal_id, "", "held = unbound");
    assert_eq!(held.page_id, "default", "placement is kept");
}

/// Review fixes 5: a new confirmed session opening on a held row's old
/// terminal id does not supersede the held row — it no longer holds that
/// terminal.
#[test]
fn record_open_on_a_held_rows_old_terminal_does_not_close_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seen = Utc::now().timestamp_millis() - 60_000;
    let mut held = fixture_rec("held", "open", seen, None, None);
    held.terminal_id = "term-old".to_string();
    held.confirmed_at = Some(seen);
    held.origin = Some("authoritative".to_string());
    write_fixture(&path, vec![held]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let now = Utc::now().timestamp_millis();
    store.mark_awaiting_account("held", now, &NONE_LIVE);

    let mut incoming = rec("new-session");
    incoming.terminal_id = "term-old".to_string();
    incoming.origin = Some("authoritative".to_string());
    incoming.confirmed_at = Some(now);
    store.record_open(incoming);

    let held = store.get("held").unwrap();
    assert_eq!(held.state, "open", "not superseded");
    assert_eq!(held.close_reason, None);
    assert!(awaiting_account_hold(&held, now), "still held");
    assert_eq!(
        store
            .find_open_by_terminal("term-old")
            .unwrap()
            .claude_session_id,
        "new-session"
    );
}

/// Review fixes 5: the poll never releases a hold through the held row's
/// OLD terminal id once that id hosts another session — the held row is
/// unbound, so the match is never `Direct`.
#[test]
fn the_poll_never_releases_a_hold_via_a_terminal_now_hosting_another_session() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seen = Utc::now().timestamp_millis() - 60_000;
    let mut held = fixture_rec("held", "open", seen, None, None);
    held.terminal_id = "term-reused".to_string();
    held.confirmed_at = Some(seen);
    write_fixture(&path, vec![held]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let now = Utc::now().timestamp_millis();
    store.mark_awaiting_account("held", now, &NONE_LIVE);
    let mut other = rec("other");
    other.terminal_id = "term-reused".to_string();
    store.record_open(other);

    let rec = store.get("held").unwrap();
    let live = |t: &str| t == "term-reused";
    let terminal_match = PollTerminalMatch::for_record(&rec, live, false);
    assert_eq!(terminal_match, PollTerminalMatch::Unmatched);
    let (mut dead, mut no_match) = (HashMap::new(), HashMap::new());
    assert!(
        poll_holds_unmatched_row(
            &store,
            "held",
            terminal_match,
            now,
            &mut dead,
            &mut no_match
        ),
        "still held"
    );
    assert!(awaiting_account_hold(&store.get("held").unwrap(), now));
}

/// Review fixes 5: a bind ends the hold — a verified resume's `record_open`
/// and a `rebind_terminal` both re-bind the row and clear the stamp, so a
/// bound row is never a held one.
#[test]
fn a_bind_ends_the_hold() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seen = Utc::now().timestamp_millis() - 60_000;
    let mut a = fixture_rec("resumed", "open", seen, None, None);
    a.terminal_id = "term-a".to_string();
    let mut b = fixture_rec("rebound", "open", seen, None, None);
    b.terminal_id = "term-b".to_string();
    write_fixture(&path, vec![a, b]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let now = Utc::now().timestamp_millis();
    store.mark_awaiting_account("resumed", now, &NONE_LIVE);
    store.mark_awaiting_account("rebound", now, &NONE_LIVE);

    let mut resume = rec("resumed");
    resume.terminal_id = "term-new-a".to_string();
    store.record_open(resume);
    store.rebind_terminal("rebound", "term-new-b", 3);
    for (id, terminal) in [("resumed", "term-new-a"), ("rebound", "term-new-b")] {
        let got = store.get(id).unwrap();
        assert_eq!(got.terminal_id, terminal, "{id}");
        assert_eq!(got.awaiting_account_since, None, "{id}: hold ended");
    }
}

/// Review fixes 5: a held row persisted with its old terminal (a file
/// written before the rule) is detached on load; a row whose stamp has
/// expired is no hold and keeps its binding.
#[test]
fn a_held_row_loaded_from_disk_is_detached() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let mut held = fixture_rec("held", "open", now - 60_000, None, None);
    held.terminal_id = "term-held".to_string();
    held.awaiting_account_since = Some(now - 60_000);
    let mut expired = fixture_rec("expired", "open", now - 60_000, None, None);
    expired.terminal_id = "term-expired".to_string();
    expired.awaiting_account_since = Some(now - OPEN_STALE_MS - 60_000);
    write_fixture(&path, vec![held, expired]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(store.get("held").unwrap().terminal_id, "");
    assert_eq!(store.get("expired").unwrap().terminal_id, "term-expired");
    drop(store);
    // Read the RAW file, not a reopened store: a reopen detaches again at
    // load, so it could not tell a persisted detach from a re-derived one.
    let on_disk = load_map(&path);
    assert_eq!(on_disk["held"].terminal_id, "", "the detach was persisted");
    assert_eq!(on_disk["expired"].terminal_id, "term-expired");
}

/// Review fixes 6, item 2: a finished row is never held — the mark is a
/// no-op that stamps nothing and detaches nothing.
#[test]
fn mark_awaiting_account_is_a_no_op_on_a_finished_row() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let mut done = fixture_rec("done", "open", now - 60_000, None, None);
    done.terminal_id = "term-done".to_string();
    done.finished_at = Some(now - 30_000);
    write_fixture(&path, vec![done]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let wal = wal_path_for(&path);
    let wal_len = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    let before = wal_len();
    store.mark_awaiting_account("done", now, &NONE_LIVE);
    let got = store.get("done").unwrap();
    assert_eq!(
        got.awaiting_account_since, None,
        "no hold on a finished row"
    );
    assert_eq!(got.terminal_id, "term-done", "nothing detached");
    assert_eq!(wal_len(), before, "nothing written");
}

/// Review fixes 6, item 4: a `record_open` that leaves a held row unbound
/// (blank incoming terminal) is not a bind — the hold stays; one carrying a
/// terminal ends it.
#[test]
fn record_open_ends_the_hold_only_when_it_binds_a_terminal() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let mut held = fixture_rec("held", "open", now - 60_000, None, None);
    held.terminal_id = "term-old".to_string();
    write_fixture(&path, vec![held]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("held", now, &NONE_LIVE);

    let mut unbound = rec("held");
    unbound.terminal_id = String::new();
    store.record_open(unbound);
    let got = store.get("held").unwrap();
    assert_eq!(got.terminal_id, "");
    assert!(
        got.awaiting_account_since.is_some(),
        "a blank re-record keeps the hold"
    );

    let mut bound = rec("held");
    bound.terminal_id = "term-new".to_string();
    store.record_open(bound);
    let got = store.get("held").unwrap();
    assert_eq!(got.terminal_id, "term-new");
    assert_eq!(got.awaiting_account_since, None, "a bind ends the hold");
}

/// Review fixes 6, item 2: a rebind to a BLANK terminal is not a bind —
/// the hold stays.
#[test]
fn rebind_to_a_blank_terminal_keeps_the_hold() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let mut held = fixture_rec("held", "open", now - 60_000, None, None);
    held.terminal_id = "term-old".to_string();
    write_fixture(&path, vec![held]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("held", now, &NONE_LIVE);
    store.rebind_terminal("held", "", 2);
    let got = store.get("held").unwrap();
    assert!(awaiting_account_hold(&got, now), "the hold is kept");
    assert_eq!(got.terminal_id, "", "still unbound");
}

/// Review fixes 6, item 1 (backend guard): the store declines to hold — and
/// so to detach — a row whose terminal is a live PTY of this process.
#[test]
fn mark_awaiting_account_declines_a_row_on_a_live_terminal() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let mut alive = fixture_rec("alive", "open", now - 60_000, None, None);
    alive.terminal_id = "term-live".to_string();
    let mut gone = fixture_rec("gone", "open", now - 60_000, None, None);
    gone.terminal_id = "term-dead".to_string();
    write_fixture(&path, vec![alive, gone]);
    let store = SessionLifecycleStore::open(&path).unwrap();
    let live = |t: &str| t == "term-live";
    store.mark_awaiting_account("alive", now, &live);
    store.mark_awaiting_account("gone", now, &live);
    let alive = store.get("alive").unwrap();
    assert_eq!(
        alive.terminal_id, "term-live",
        "a live session is never detached"
    );
    assert_eq!(alive.awaiting_account_since, None, "nor held");
    let gone = store.get("gone").unwrap();
    assert_eq!(
        gone.terminal_id, "",
        "a dead terminal's row is held and detached"
    );
    assert!(awaiting_account_hold(&gone, now));
}

/// Review fixes 5, NIT 1: a re-mark that changes nothing writes nothing —
/// a running hold on an unbound row, and an expired stamp re-stamped to the
/// same `last_seen_at` (the row was not seen since).
#[test]
fn a_re_mark_that_changes_nothing_does_not_persist() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let now = Utc::now().timestamp_millis();
    let stale_seen = now - OPEN_STALE_MS - 60_000;
    let mut stale = fixture_rec("stale", "open", stale_seen, None, None);
    stale.terminal_id = "term-stale".to_string();
    stale.awaiting_account_since = Some(stale_seen);
    write_fixture(
        &path,
        vec![fixture_rec("held", "open", now - 60_000, None, None), stale],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    store.mark_awaiting_account("held", now, &NONE_LIVE);
    let wal = wal_path_for(&path);
    let wal_len = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    let before = wal_len();
    store.mark_awaiting_account("held", now, &NONE_LIVE);
    store.mark_awaiting_account("stale", now, &NONE_LIVE);
    assert_eq!(wal_len(), before, "no delta appended");
    let stale = store.get("stale").unwrap();
    assert_eq!(stale.awaiting_account_since, Some(stale_seen));
    assert_eq!(
        stale.terminal_id, "term-stale",
        "an expired hold detaches nothing"
    );
}

// --- withheld boot restore (plan 2026-10-01-drain-deferred-restore-is-swept-as-orphans)

/// Drive the lifecycle poll's no-match path for `ticks` ticks exactly as
/// `main.rs` does for records that match NO live terminal (every prior-boot
/// record after a crash): `classify` → `hold_for_withheld_boot_restore` →
/// the same counter bookkeeping and `record_close(.., "no-terminal")`.
fn simulate_no_terminal_poll_ticks(
    store: &SessionLifecycleStore,
    ticks: u32,
    boot_at_ms: Option<i64>,
    phase: BootRestorePhase,
    drain_defers_boot_resume: bool,
) {
    simulate_no_terminal_poll_ticks_with_census(
        store,
        ticks,
        boot_at_ms,
        phase,
        drain_defers_boot_resume,
        None,
    );
}

/// [`simulate_no_terminal_poll_ticks`] with the boot census latched, as
/// `main.rs` reads it each tick ([`crate::session::restore_census::latched`]).
fn simulate_no_terminal_poll_ticks_with_census(
    store: &SessionLifecycleStore,
    ticks: u32,
    boot_at_ms: Option<i64>,
    phase: BootRestorePhase,
    drain_defers_boot_resume: bool,
    census: Option<&crate::session::restore_census::BootCensus>,
) {
    let mut consecutive_no_match: HashMap<String, u32> = HashMap::new();
    for _ in 0..ticks {
        for rec in store.open_records() {
            let prior_no_match = consecutive_no_match
                .get(&rec.claude_session_id)
                .copied()
                .unwrap_or(0);
            let held = boot_restore_hold(
                rec.last_seen_at,
                boot_at_ms,
                phase,
                drain_defers_boot_resume,
                census.is_some_and(|c| c.awaits_restore(&rec)),
            )
            .hold;
            let action = hold_for_withheld_boot_restore(
                classify(
                    None,
                    false,
                    0,
                    prior_no_match,
                    true,
                    rec.restore_pending_at.is_some(),
                    rec.confirmed_at.is_some(),
                    WorkerPlane::NotWorker,
                ),
                held,
            );
            if held && action == PollAction::Skip {
                consecutive_no_match.remove(&rec.claude_session_id);
            }
            match action {
                PollAction::NoMatchWait => {
                    consecutive_no_match.insert(rec.claude_session_id.clone(), prior_no_match + 1);
                }
                PollAction::CloseNoTerminal => {
                    store.record_close(&rec.claude_session_id, "no-terminal");
                    consecutive_no_match.remove(&rec.claude_session_id);
                }
                PollAction::Skip => {}
                other => panic!("no-match path produced {other:?}"),
            }
        }
    }
}

/// The 2026-10-01 registry shape: a confirmed crash cohort last seen at the
/// crash, an old ghost, and (optionally) a session this boot has kept alive
/// for hours — which is what drags the unpartitioned anchor forward.
fn crash_cohort_store(
    dir: &Path,
    crash: i64,
    this_boot_live_seen_at: Option<i64>,
) -> SessionLifecycleStore {
    let path = dir.join("terminal-sessions.json");
    let confirmed = |mut r: TerminalSessionRecord| {
        r.confirmed_at = Some(r.opened_at);
        r
    };
    let mut recs = vec![
        confirmed(fixture_rec("victim-a", "open", crash, None, None)),
        confirmed(fixture_rec("victim-b", "open", crash - 40_000, None, None)),
        confirmed(fixture_rec(
            "ghost",
            "open",
            crash - 72 * 3_600_000,
            None,
            None,
        )),
    ];
    if let Some(seen) = this_boot_live_seen_at {
        recs.push(confirmed(fixture_rec(
            "this-boot-live",
            "open",
            seen,
            None,
            None,
        )));
    }
    write_fixture(&path, recs);
    SessionLifecycleStore::open(&path).unwrap()
}

/// `boot_at_ms: None` models the unpartitioned rule: every row reads as
/// the prior life (the test-default boot line, `i64::MAX`), so the prior
/// anchor is the max over every row.
fn sorted_restorable(store: &SessionLifecycleStore, boot_at_ms: Option<i64>) -> Vec<String> {
    store.set_boot_ms_for_test(boot_at_ms.unwrap_or(i64::MAX));
    // A crash boot: the crashed process's marker never feeds the anchor.
    let mut ids: Vec<String> = store
        .restorable_records(None, false)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    ids.sort();
    ids
}

/// (a) Drained boot: the crash cohort survives far more than
/// `NO_TERMINAL_ORPHAN_TICKS` poll ticks still `open` and still restorable
/// — both before the frontend first asks (`NotYetRun` + gate deferring) and
/// after the restore path recorded the deferral (`Withheld`).
#[test]
fn drained_boot_holds_crash_cohort_open_and_restorable() {
    for phase in [BootRestorePhase::NotYetRun, BootRestorePhase::Withheld] {
        let dir = tempdir().unwrap();
        let crash = 1_700_000_000_000_i64;
        let boot = crash + 30_000;
        let store = crash_cohort_store(dir.path(), crash, None);

        simulate_no_terminal_poll_ticks(
            &store,
            NO_TERMINAL_ORPHAN_TICKS * 10,
            Some(boot),
            phase,
            true,
        );

        for id in ["victim-a", "victim-b", "ghost"] {
            assert_eq!(
                store.get(id).unwrap().state,
                "open",
                "{phase:?}: {id} must not be closed while its restore is withheld"
            );
        }
        assert_eq!(
            sorted_restorable(&store, Some(boot)),
            vec!["victim-a".to_string(), "victim-b".to_string()],
            "{phase:?}: the crash cohort is still restorable (the ghost never was)"
        );
    }
}

/// The pre-fix behaviour this replaces, pinned so the test above is not a
/// coincidence: with no hold the same ticks close every victim
/// `no-terminal`, and the restore set comes back empty.
#[test]
fn without_the_hold_a_drained_boot_strands_the_cohort() {
    let dir = tempdir().unwrap();
    let crash = 1_700_000_000_000_i64;
    let boot = crash + 30_000;
    let store = crash_cohort_store(dir.path(), crash, None);
    // `Ran` disables the hold; nothing restored them, so they are swept.
    simulate_no_terminal_poll_ticks(
        &store,
        NO_TERMINAL_ORPHAN_TICKS + 1,
        Some(boot),
        BootRestorePhase::Ran,
        true,
    );
    let victim = store.get("victim-a").unwrap();
    assert_eq!(victim.state, "closed");
    assert_eq!(victim.close_reason.as_deref(), Some("no-terminal"));
    assert!(sorted_restorable(&store, Some(boot)).is_empty());
}

/// (b) The drain lifts hours later, after this boot has kept a session
/// alive the whole time: the restore set still contains the crash cohort
/// (the boot-partitioned anchor), and once the restore marks them pending
/// the resumed poll (`Ran`) leaves them alone while still retiring the
/// genuine orphan it did not restore.
#[test]
fn after_the_drain_lifts_the_restore_set_contains_the_crash_cohort() {
    let dir = tempdir().unwrap();
    let crash = 1_700_000_000_000_i64;
    let boot = crash + 30_000;
    let lift = boot + 4 * 3_600_000; // a four-hour drain
    let store = crash_cohort_store(dir.path(), crash, None);

    // Hours of drained ticks.
    simulate_no_terminal_poll_ticks(&store, 300, Some(boot), BootRestorePhase::Withheld, true);

    // Meanwhile this boot kept a session alive the whole time. It is added
    // AFTER the simulated ticks because the simulator models every record
    // as matching no terminal — a live this-boot row would be (correctly,
    // see (c)) closed `no-terminal` by it, which is not this scenario.
    let path = dir.path().join("terminal-sessions.json");
    age_persisted_records(&store, &path, |m| {
        let mut live = fixture_rec("this-boot-live", "open", lift - 20_000, None, None);
        live.confirmed_at = Some(live.opened_at);
        m.insert(live.claude_session_id.clone(), live);
    });
    let store = SessionLifecycleStore::open(&path).unwrap();

    let restore_set = sorted_restorable(&store, Some(boot));
    assert_eq!(
        restore_set,
        vec![
            "this-boot-live".to_string(),
            "victim-a".to_string(),
            "victim-b".to_string(),
        ],
        "the crash cohort is judged against the PRIOR boot's anchor"
    );
    // Unpartitioned, the hours-later live row drags the anchor past the
    // cohort — the latent second strand this fix also closes.
    assert_eq!(
        sorted_restorable(&store, None),
        vec!["this-boot-live".to_string()],
    );

    // The frontend restores what it was handed.
    store.mark_restore_pending("victim-a", true);
    store.mark_restore_pending("victim-b", true);
    simulate_no_terminal_poll_ticks(
        &store,
        NO_TERMINAL_ORPHAN_TICKS + 2,
        Some(boot),
        BootRestorePhase::Ran,
        false,
    );
    assert_eq!(store.get("victim-a").unwrap().state, "open");
    assert_eq!(store.get("victim-b").unwrap().state, "open");
    let ghost = store.get("ghost").unwrap();
    assert_eq!(ghost.state, "closed", "normal orphan handling resumed");
    assert_eq!(ghost.close_reason.as_deref(), Some("no-terminal"));
}

/// (c) Genuine orphans still close when the restore is not being withheld:
/// an undrained boot that has not run its restore yet, and a this-boot
/// record even while the boot restore IS withheld.
#[test]
fn genuine_orphans_still_close_when_not_withheld() {
    let crash = 1_700_000_000_000_i64;
    let boot = crash + 30_000;

    // Not drained, restore not yet run: unchanged behaviour.
    let dir = tempdir().unwrap();
    let store = crash_cohort_store(dir.path(), crash, None);
    simulate_no_terminal_poll_ticks(
        &store,
        NO_TERMINAL_ORPHAN_TICKS + 1,
        Some(boot),
        BootRestorePhase::NotYetRun,
        false,
    );
    assert_eq!(store.get("ghost").unwrap().state, "closed");
    assert_eq!(store.get("victim-a").unwrap().state, "closed");

    // Withheld, but the record lived in THIS boot: judged normally.
    let dir = tempdir().unwrap();
    let store = crash_cohort_store(dir.path(), crash, Some(boot + 60_000));
    simulate_no_terminal_poll_ticks(
        &store,
        NO_TERMINAL_ORPHAN_TICKS + 1,
        Some(boot),
        BootRestorePhase::Withheld,
        true,
    );
    let live = store.get("this-boot-live").unwrap();
    assert_eq!(live.state, "closed");
    assert_eq!(live.close_reason.as_deref(), Some("no-terminal"));
    assert_eq!(store.get("victim-a").unwrap().state, "open");
}

#[test]
fn withheld_boot_restore_candidate_truth_table() {
    let boot = Some(1_000);
    use BootRestorePhase::*;
    // Prior-boot record outside the boot-latched restorable set.
    assert!(withheld_boot_restore_candidate(
        999, boot, Withheld, false, false
    ));
    assert!(withheld_boot_restore_candidate(
        999, boot, Withheld, true, false
    ));
    assert!(withheld_boot_restore_candidate(
        999, boot, NotYetRun, true, false
    ));
    assert!(!withheld_boot_restore_candidate(
        999, boot, NotYetRun, false, false
    ));
    assert!(!withheld_boot_restore_candidate(
        999, boot, Ran, true, false
    ));
    // Plan 2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-
    // restore-set, Phase 4: a prior-boot row still awaiting the boot
    // restore that latched it is held in EVERY phase. The `Ran` row is the
    // unopened page: the restore ran for the page on screen, not for this
    // row's.
    assert!(withheld_boot_restore_candidate(999, boot, Ran, false, true));
    assert!(withheld_boot_restore_candidate(
        999, boot, NotYetRun, false, true
    ));
    // This-boot record: never held, awaiting or not.
    assert!(!withheld_boot_restore_candidate(
        1_000, boot, Withheld, true, false
    ));
    assert!(!withheld_boot_restore_candidate(
        1_000, boot, Ran, false, true
    ));
    // No boot instant: nothing is provably prior-boot.
    assert!(!withheld_boot_restore_candidate(
        999, None, Withheld, true, false
    ));
    assert!(!withheld_boot_restore_candidate(
        999, None, Ran, false, true
    ));
}

/// Plan 2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-
/// restore-set, Phase 4: the boot-census arm widens the HOLD, never the
/// MATCH. After the restore ran, a latched row whose own terminal id is
/// stale still finds its live tab through the `(page_id, title,
/// working_dir)` fallback, as it did before the hold existed. The
/// drain/phase hold still narrows the match to the id.
#[test]
fn the_census_hold_never_narrows_the_live_terminal_match() {
    use BootRestorePhase::*;
    let boot = Some(1_000);

    let waiting = boot_restore_hold(999, boot, Ran, false, true);
    assert!(waiting.hold, "a latched prior-boot row is held after Ran");
    assert!(
        !waiting.id_match_only,
        "the census arm must not disable the triple fallback"
    );

    let withheld = boot_restore_hold(999, boot, Withheld, false, false);
    assert!(withheld.hold && withheld.id_match_only);

    let neither = boot_restore_hold(999, boot, Ran, false, false);
    assert!(!neither.hold && !neither.id_match_only);

    let triple = ("page-1", "claude", "D:/work");
    let live_tab = "live-tab";
    let by_id: HashMap<&str, &str> = HashMap::new();
    let by_triple: HashMap<(&str, &str, &str), &str> = HashMap::from([(triple, live_tab)]);
    assert_eq!(
        match_live_terminal(
            "stale-term",
            Some(triple),
            &by_id,
            &by_triple,
            waiting.id_match_only
        ),
        Some(live_tab),
        "a waiting row keeps its live tab"
    );
    assert_eq!(
        match_live_terminal(
            "stale-term",
            Some(triple),
            &by_id,
            &by_triple,
            withheld.id_match_only
        ),
        None,
        "a withheld row matches its own id only"
    );
}

/// Plan 2026-10-28-force-exited-runner-strands-open-sessions-out-of-the-
/// restore-set, Phase 4: the boot restore has RUN (for the page on
/// screen), and the boot-latched cohort sits on a page never opened, so it
/// matches no terminal. The poll must hold it `open` past the orphan
/// debounce, while the ghost the latch left out (a genuine orphan) still
/// closes `no-terminal`. Finishing a held row releases the hold.
#[test]
fn a_latched_row_on_an_unopened_page_is_not_swept_as_an_orphan() {
    let dir = tempdir().unwrap();
    let crash = 1_700_000_000_000_i64;
    let boot = crash + 30_000;
    let store = crash_cohort_store(dir.path(), crash, None);
    store.set_boot_ms_for_test(boot);
    // The census latches the restorable set, exactly as main.rs does at
    // boot (a crash boot: the crashed process's marker never anchors).
    #[derive(Debug)]
    struct NoTranscripts;
    impl TranscriptProbe for NoTranscripts {
        fn transcript_exists(&self, _id: &str, _wd: Option<&str>) -> bool {
            false
        }
    }
    let census = crate::session::restore_census::BootCensus {
        boot_at_ms: boot,
        expected: crate::session::restore_census::expected_rows(
            store.restorable_records(None, false),
            &NoTranscripts,
        ),
    };
    let latched: Vec<&str> = census
        .expected
        .iter()
        .map(|e| e.claude_session_id.as_str())
        .collect();
    assert_eq!(
        latched,
        vec!["victim-a", "victim-b"],
        "the ghost is not latched"
    );

    simulate_no_terminal_poll_ticks_with_census(
        &store,
        NO_TERMINAL_ORPHAN_TICKS + 2,
        Some(boot),
        BootRestorePhase::Ran,
        false,
        Some(&census),
    );
    assert_eq!(store.get("victim-a").unwrap().state, "open");
    assert_eq!(store.get("victim-b").unwrap().state, "open");
    let ghost = store.get("ghost").unwrap();
    assert_eq!(ghost.state, "closed", "a genuine orphan still closes");
    assert_eq!(ghost.close_reason.as_deref(), Some("no-terminal"));
    assert_eq!(
        sorted_restorable(&store, Some(boot)),
        vec!["victim-a".to_string(), "victim-b".to_string()],
        "the held cohort is still restorable when its page opens"
    );

    // The operator finishes one: it no longer awaits a restore, so the
    // ordinary orphan handling retires it.
    store
        .set_finished("victim-b", true, None)
        .expect("a known session");
    simulate_no_terminal_poll_ticks_with_census(
        &store,
        NO_TERMINAL_ORPHAN_TICKS + 2,
        Some(boot),
        BootRestorePhase::Ran,
        false,
        Some(&census),
    );
    assert_eq!(store.get("victim-a").unwrap().state, "open");
    assert_eq!(store.get("victim-b").unwrap().state, "closed");
}

/// Same plan, Phase 4: the hold survives a SECOND restart before the page
/// opens. Process A held `held` unjudged on its last successful tick and
/// served another session that died at A's end, so A's epoch and every
/// anchor sit hours past `held`'s stamp. Boot B admits `held` by the
/// carried hold, while an open row A did not hold, with the same old
/// stamp, stays excluded. (Stamps are relative to now: the hold expires at
/// the open-row prune age, measured against wall-clock now.)
#[test]
fn a_held_row_survives_a_second_restart_before_its_page_opens() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let hour = 3_600_000_i64;
    let now = Utc::now().timestamp_millis();
    let (stamp, boot_a, a_end, boot_b) =
        (now - 10 * hour, now - 9 * hour, now - 2 * hour, now - hour);
    write_fixture(
        &path,
        vec![
            fixture_rec("held", "open", stamp, None, None),
            fixture_rec("not-held", "open", stamp, None, None),
            fixture_rec("served", "closed", a_end, Some(a_end), Some("poll-dead")),
        ],
    );
    let epoch_path = dir
        .path()
        .join(crate::session::observation_epoch::EPOCH_FILE_NAME);
    assert!(ObservationEpoch::open(&epoch_path, boot_a).record_tick(
        true,
        a_end,
        &["held".to_string()]
    ));

    let store = SessionLifecycleStore::open(&path).unwrap();
    store.set_boot_ms_for_test(boot_b);
    store.attach_prior_teardown_complete(false);
    store.attach_observation_epoch(Arc::new(ObservationEpoch::open(&epoch_path, boot_b)));

    assert_eq!(
        restorable_ids(&store, None, false),
        vec!["held".to_string(), "served".to_string()],
        "the carried hold admits `held`; the unheld row of the same age stays out"
    );
}

/// Same plan, Phase 4: each release condition of the carried hold, alone.
#[test]
fn awaiting_prior_boot_restore_truth_table() {
    let hour = 3_600_000_i64;
    let (boot, now) = (100 * hour, 101 * hour);
    let held: HashSet<&str> = HashSet::from(["r"]);
    let row = |state: &str, last_seen_at: i64| fixture_rec("r", state, last_seen_at, None, None);

    assert!(awaiting_prior_boot_restore(
        &row("open", boot - hour),
        &held,
        boot,
        now
    ));
    // Not in the carried set.
    assert!(!awaiting_prior_boot_restore(
        &row("open", boot - hour),
        &HashSet::new(),
        boot,
        now
    ));
    // Re-stamped at or after this boot (a restore re-asserted it): judged
    // by recency again.
    assert!(!awaiting_prior_boot_restore(
        &row("open", boot),
        &held,
        boot,
        now
    ));
    // Exactly at the open-row prune age: still held; past it: not.
    assert!(awaiting_prior_boot_restore(
        &row("open", now - OPEN_STALE_MS),
        &held,
        boot,
        now
    ));
    assert!(!awaiting_prior_boot_restore(
        &row("open", now - OPEN_STALE_MS - 1),
        &held,
        boot,
        now
    ));
    // Closed or finished rows are never held.
    assert!(!awaiting_prior_boot_restore(
        &row("closed", boot - hour),
        &held,
        boot,
        now
    ));
    let mut finished = row("open", boot - hour);
    finished.finished_at = Some(boot - hour);
    assert!(!awaiting_prior_boot_restore(&finished, &held, boot, now));
}

/// Same plan, Phase 4 (pre-PR review rounds 3-4): the poll carries a held
/// census row while its restore has not been attempted in this process,
/// or was attempted less than `RESTORE_PENDING_TTL_MS` ago (a resume may
/// still be verifying on a loaded box). Past the TTL (e.g. a failed resume
/// whose tab the operator closed) it is not re-offered on every restart.
#[test]
fn carries_boot_restore_hold_only_until_this_boots_attempt_expires() {
    let boot = Some(1_000);
    let ttl = RESTORE_PENDING_TTL_MS;
    let now = 1_000 + 10 * ttl;
    let mut r = fixture_rec("r", "open", 999, None, None);
    assert!(carries_boot_restore_hold(&r, boot, true, now));
    assert!(
        !carries_boot_restore_hold(&r, boot, false, now),
        "not a census row"
    );
    r.restore_pending_at = Some(500);
    assert!(
        carries_boot_restore_hold(&r, boot, true, now),
        "a pending marker left by an earlier process still awaits THIS boot's restore"
    );
    r.restore_pending_at = Some(1_000);
    assert!(
        !carries_boot_restore_hold(&r, boot, true, now),
        "attempted this boot, past the TTL: a resume that did not verify"
    );
    assert!(
        carries_boot_restore_hold(&r, boot, true, 1_000 + ttl),
        "attempted this boot, still inside the TTL: may still be verifying"
    );
    assert!(
        !carries_boot_restore_hold(&r, boot, true, 1_000 + ttl + 1),
        "one millisecond past the TTL"
    );
    assert!(
        carries_boot_restore_hold(&r, None, true, now),
        "no boot instant: no attempt is provably this boot's"
    );
}

#[test]
fn hold_rewrites_only_the_no_match_arms() {
    use PollAction::*;
    assert_eq!(hold_for_withheld_boot_restore(NoMatchWait, true), Skip);
    assert_eq!(hold_for_withheld_boot_restore(CloseNoTerminal, true), Skip);
    for a in [KeepAlive, NeedsConfirm, Close, CloseNeverStarted, Skip] {
        assert_eq!(hold_for_withheld_boot_restore(a, true), a);
    }
    for a in [NoMatchWait, CloseNoTerminal, KeepAlive, Close] {
        assert_eq!(hold_for_withheld_boot_restore(a, false), a);
    }
}

/// The latch is monotonic: a repeated deferral is idempotent, `Ran` is
/// terminal, and a deferral after the restore ran does not re-arm the hold.
/// The only test that touches the process-global latch.
#[test]
fn boot_restore_phase_latch_is_monotonic() {
    reset_boot_restore_phase_for_test();
    assert_eq!(boot_restore_phase(), BootRestorePhase::NotYetRun);
    note_boot_restore_withheld();
    note_boot_restore_withheld();
    assert_eq!(boot_restore_phase(), BootRestorePhase::Withheld);
    note_boot_restore_ran();
    assert_eq!(boot_restore_phase(), BootRestorePhase::Ran);
    note_boot_restore_withheld();
    assert_eq!(boot_restore_phase(), BootRestorePhase::Ran);
    reset_boot_restore_phase_for_test();
}

/// Clean boot: the shutdown marker (written before this boot) joins the
/// PRIOR-boot anchor, so the cohort stays admitted however long this boot
/// has kept another session alive; unpartitioned, that session drags the
/// anchor past the cohort.
#[test]
fn clean_boot_partition_counts_the_marker_in_the_prior_anchor() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let shutdown = 1_700_000_000_000_i64;
    let boot = shutdown + 30_000;
    let later = boot + 3 * 3_600_000;
    let confirmed = |mut r: TerminalSessionRecord| {
        r.confirmed_at = Some(r.opened_at);
        r
    };
    write_fixture(
        &path,
        vec![
            confirmed(fixture_rec("prior", "open", shutdown - 120_000, None, None)),
            // 11 min before the marker: excluded ONLY because the marker
            // counts. Without it the prior anchor is `prior`'s own stamp
            // (9 min away) and this row would be admitted.
            confirmed(fixture_rec(
                "stale-prior",
                "open",
                shutdown - 660_000,
                None,
                None,
            )),
            confirmed(fixture_rec("this-boot", "open", later, None, None)),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    let ids = |boot_at: Option<i64>| {
        store.set_boot_ms_for_test(boot_at.unwrap_or(i64::MAX));
        let mut v: Vec<String> = store
            .restorable_records(Some(shutdown), true)
            .into_iter()
            .map(|r| r.claude_session_id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        ids(Some(boot)),
        vec!["prior".to_string(), "this-boot".to_string()]
    );
    assert_eq!(ids(None), vec!["this-boot".to_string()]);
}

/// Vet correction 1: only the boot restore's own read moves the latch, in
/// either direction. A tree-reset report, worker-adoption probe or page
/// reconcile read — deferred or not — leaves it untouched, so none of them
/// can release the poll's hold before anything was restored.
#[test]
fn only_the_boot_restore_read_moves_the_latch() {
    for deferred in [true, false] {
        assert_eq!(
            boot_restore_latch_event(false, deferred),
            BootRestoreLatchEvent::Untouched,
            "a non-restore read (deferred={deferred}) must not move the latch"
        );
    }
    assert_eq!(
        boot_restore_latch_event(true, true),
        BootRestoreLatchEvent::Withheld
    );
    assert_eq!(
        boot_restore_latch_event(true, false),
        BootRestoreLatchEvent::Ran
    );
}

/// The hold's `drain_defers_boot_resume` input is `!drain_gate(BootResume)
/// .allows()`. Pin that BOTH a drain and an UNKNOWN drain state defer the
/// boot restore (so they hold), and that a clear/expired drain does not.
#[test]
fn boot_resume_gate_defers_on_drained_and_unknown_drain_state() {
    use crate::coord_drain_state::{gate_for_at, CoordDrainState, SpawnOrigin};
    let now = Utc::now();
    let defers = |st: CoordDrainState| !gate_for_at(&st, SpawnOrigin::BootResume, now).allows();
    assert!(defers(CoordDrainState::Drained {
        until: Some(now + chrono::Duration::hours(3)),
        reason: None,
    }));
    assert!(defers(CoordDrainState::Unknown {
        since: now,
        cause: "unreadable".to_string(),
    }));
    assert!(!defers(CoordDrainState::Clear));
    assert!(!defers(CoordDrainState::Drained {
        until: Some(now - chrono::Duration::minutes(1)),
        reason: None,
    }));
}

/// A held record gets the id match only: a this-boot terminal that merely
/// shares its page/title/working dir must not be taken for it.
#[test]
fn held_record_ignores_the_triple_fallback_match() {
    let live = "live-terminal";
    let mut by_id: HashMap<&str, &str> = HashMap::new();
    by_id.insert("term-now", live);
    let mut by_triple: HashMap<(&str, &str, &str), &str> = HashMap::new();
    by_triple.insert(("default", "Claude 1", "/repo"), live);
    let triple = Some(("default", "Claude 1", "/repo"));

    assert_eq!(
        match_live_terminal("term-dead", triple, &by_id, &by_triple, false),
        Some("live-terminal"),
        "unheld: the fallback still applies"
    );
    assert_eq!(
        match_live_terminal("term-dead", triple, &by_id, &by_triple, true),
        None,
        "held: no fallback"
    );
    assert_eq!(
        match_live_terminal("term-now", triple, &by_id, &by_triple, true),
        Some("live-terminal"),
        "held: an exact id match still counts"
    );
}

/// A gone worker record's `CloseNoTerminal` is the same variant, so the hold
/// keeps a prior-boot worker row open through the drain too — parity with
/// an undrained boot, where the restore runs before the first poll tick.
#[test]
fn hold_covers_a_gone_prior_boot_worker_row() {
    let action = classify(None, false, 0, 0, true, false, true, WorkerPlane::Gone);
    assert_eq!(action, PollAction::CloseNoTerminal);
    assert_eq!(
        hold_for_withheld_boot_restore(action, true),
        PollAction::Skip
    );
}

/// This-boot CLOSES (the boot repair stamps `closed_at = now`, and any
/// session that ends during the drain) do not move the prior-boot anchor.
#[test]
fn this_boot_closes_do_not_move_the_prior_boot_anchor() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let crash = 1_700_000_000_000_i64;
    let boot = crash + 30_000;
    let mut victim = fixture_rec("victim", "open", crash, None, None);
    victim.confirmed_at = Some(victim.opened_at);
    write_fixture(
        &path,
        vec![
            victim,
            fixture_rec(
                "repaired",
                "closed",
                crash - 600_000,
                Some(boot + 1_000),
                Some("superseded-terminal-reuse"),
            ),
            fixture_rec(
                "ended-during-drain",
                "closed",
                boot + 2 * 3_600_000,
                Some(boot + 2 * 3_600_000),
                Some("user-close"),
            ),
        ],
    );
    let store = SessionLifecycleStore::open(&path).unwrap();
    let later = boot + 3 * 3_600_000;
    assert_eq!(
        sorted_restorable(&store, Some(boot)),
        vec!["victim".to_string()]
    );
    assert!(sorted_restorable(&store, None).is_empty());
}
