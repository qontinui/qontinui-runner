//! Tests for [`SessionLifecycleStore::sync_zone`] (plan
//! `2026-11-08-tenant-badge-parity-and-oracle-fixtures` Phase 6).
//!
//! A sibling file rather than a block inside `session_lifecycle_store.rs`,
//! which is far past the served file-size ceiling (precedent:
//! `journey/routes.rs` → `routes_tests.rs`).
//!
//! The property under test: an HTTP-created session's recorded zone follows
//! the grid zone its live terminal renders in, and NOTHING else on the record
//! moves — no `last_seen_at` refresh (ghost rows would become immortal), no
//! restore tier, no `confirmed_at`, no terminal re-point.

use super::*;
use tempfile::tempdir;

const SID: &str = "11111111-2222-4333-8444-555555555555";
const LIVE_TERM: &str = "term-live";

/// An open record for [`SID`] on [`LIVE_TERM`] in zone 0 — what both backend
/// writers of a `POST /terminals` session record — with liveness and
/// provenance fields set to distinctive values so any refresh is visible.
fn http_created_record(last_seen_at: i64) -> TerminalSessionRecord {
    let mut rec = test_open_record(SID);
    rec.terminal_id = LIVE_TERM.to_string();
    rec.zone_index = 0;
    rec.opened_at = last_seen_at - 1_000;
    rec.last_seen_at = last_seen_at;
    rec.confirmed_at = Some(last_seen_at - 500);
    rec.restore_tier = Some(RESTORE_TIER_TERMINAL_ONLY.to_string());
    rec.restored_from_boot_at = Some(last_seen_at - 200);
    rec.origin = Some("observed".to_string());
    rec
}

/// Write `records` as the store's snapshot and open it. The snapshot is the
/// only way to seed `last_seen_at` / `confirmed_at` / `restore_tier` exactly
/// (every public mutator stamps "now").
fn open_seeded(path: &Path, records: Vec<TerminalSessionRecord>) -> SessionLifecycleStore {
    let m: HashMap<String, TerminalSessionRecord> = records
        .into_iter()
        .map(|r| (r.claude_session_id.clone(), r))
        .collect();
    std::fs::write(path, serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    SessionLifecycleStore::open(path).unwrap()
}

/// Deltas appended to the WAL since the last compaction — the store's own
/// count of durable writes. A no-op must leave it unmoved.
fn wal_appends(store: &SessionLifecycleStore) -> usize {
    let w = store.wal.lock().unwrap();
    assert!(!w.dirty, "a mutation failed to reach the WAL");
    w.appends
}

fn aged() -> i64 {
    Utc::now().timestamp_millis() - 600_000
}

#[test]
fn an_open_record_on_the_live_terminal_takes_the_rendered_zone() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seeded = http_created_record(aged());
    let store = open_seeded(&path, vec![seeded.clone()]);
    let appends_before = wal_appends(&store);

    let out = store.sync_zone(SID, LIVE_TERM, 2);

    assert_eq!(
        out,
        SyncZoneOutcome {
            updated: true,
            previous_zone: Some(0),
        }
    );
    let after = store.get(SID).unwrap();
    assert_eq!(
        after.zone_index, 2,
        "the recorded zone must follow the grid"
    );
    assert_eq!(
        wal_appends(&store),
        appends_before + 1,
        "exactly one durable write"
    );

    // Survives a reopen — persisted, not in-memory only.
    let reloaded = SessionLifecycleStore::open(&path).unwrap();
    assert_eq!(reloaded.get(SID).unwrap().zone_index, 2);
}

#[test]
fn a_sync_never_refreshes_liveness_or_provenance_or_the_binding() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let seeded = http_created_record(aged());
    let store = open_seeded(&path, vec![seeded.clone()]);
    let before = store.get(SID).unwrap();

    assert!(store.sync_zone(SID, LIVE_TERM, 3).updated);

    let after = store.get(SID).unwrap();
    assert_eq!(
        after.last_seen_at, before.last_seen_at,
        "sync_zone must NOT refresh last_seen_at (ghost rows would become immortal)"
    );
    assert_eq!(
        after.confirmed_at, before.confirmed_at,
        "confirmed_at untouched"
    );
    assert_eq!(
        after.restore_tier, before.restore_tier,
        "no restore tier stamped"
    );
    assert_eq!(
        after.restored_from_boot_at, before.restored_from_boot_at,
        "no restore stamp"
    );
    assert_eq!(
        after.terminal_id, LIVE_TERM,
        "the terminal is never re-pointed"
    );
    assert_eq!(after.opened_at, before.opened_at);
    assert_eq!(after.origin, before.origin);
    assert_eq!(after.state, "open");
    assert_eq!(after.page_id, before.page_id);
    // Nothing but the zone moved.
    let mut expected = before.clone();
    expected.zone_index = 3;
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "only zone_index may change"
    );
}

#[test]
fn the_same_zone_is_no_write_and_no_flush() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let mut seeded = http_created_record(aged());
    seeded.zone_index = 4;
    let store = open_seeded(&path, vec![seeded]);
    let appends_before = wal_appends(&store);
    let before = store.get(SID).unwrap();

    let out = store.sync_zone(SID, LIVE_TERM, 4);

    assert_eq!(
        out,
        SyncZoneOutcome {
            updated: false,
            previous_zone: Some(4),
        }
    );
    assert_eq!(
        wal_appends(&store),
        appends_before,
        "a same-zone sync must append nothing to the WAL"
    );
    assert_eq!(
        serde_json::to_value(store.get(SID).unwrap()).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
}

#[test]
fn a_closed_record_is_never_moved() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let mut seeded = http_created_record(aged());
    seeded.state = "closed".to_string();
    seeded.closed_at = Some(seeded.last_seen_at + 10);
    seeded.close_reason = Some("explicit".to_string());
    let store = open_seeded(&path, vec![seeded]);
    let appends_before = wal_appends(&store);

    let out = store.sync_zone(SID, LIVE_TERM, 2);

    assert_eq!(
        out,
        SyncZoneOutcome {
            updated: false,
            previous_zone: None,
        }
    );
    let after = store.get(SID).unwrap();
    assert_eq!(after.zone_index, 0, "a closed record keeps its zone");
    assert_eq!(after.state, "closed");
    assert_eq!(wal_appends(&store), appends_before);
}

#[test]
fn a_record_bound_to_another_terminal_is_never_moved() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    // An auto-resume row before its verified re-record still names the dead
    // pre-restart terminal: a zone observed on the NEW tab must not move it.
    let mut seeded = http_created_record(aged());
    seeded.terminal_id = "term-dead-pre-restart".to_string();
    let store = open_seeded(&path, vec![seeded]);
    let appends_before = wal_appends(&store);

    let out = store.sync_zone(SID, LIVE_TERM, 2);

    assert_eq!(
        out,
        SyncZoneOutcome {
            updated: false,
            previous_zone: None,
        }
    );
    let after = store.get(SID).unwrap();
    assert_eq!(after.zone_index, 0);
    assert_eq!(
        after.terminal_id, "term-dead-pre-restart",
        "sync_zone never re-points a terminal"
    );
    assert_eq!(wal_appends(&store), appends_before);
}

#[test]
fn an_unknown_session_is_a_noop_that_creates_nothing() {
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let store = open_seeded(&path, vec![http_created_record(aged())]);
    let appends_before = wal_appends(&store);

    let out = store.sync_zone("ghost-session", LIVE_TERM, 2);

    assert_eq!(
        out,
        SyncZoneOutcome {
            updated: false,
            previous_zone: None,
        }
    );
    assert!(store.get("ghost-session").is_none());
    assert_eq!(wal_appends(&store), appends_before);
}

#[test]
fn the_unzoned_index_is_a_real_target() {
    // Past the zone ceiling a live tab is genuinely unassigned; the record
    // follows it down to -1 rather than being clamped.
    let _amb = crate::test_env::isolated_ambient();
    let dir = tempdir().unwrap();
    let path = dir.path().join("terminal-sessions.json");
    let mut seeded = http_created_record(aged());
    seeded.zone_index = 5;
    let store = open_seeded(&path, vec![seeded]);

    let out = store.sync_zone(SID, LIVE_TERM, -1);

    assert_eq!(
        out,
        SyncZoneOutcome {
            updated: true,
            previous_zone: Some(5),
        }
    );
    assert_eq!(store.get(SID).unwrap().zone_index, -1);
}

#[test]
fn the_outcome_serializes_as_updated_and_previous_zone() {
    let v = serde_json::to_value(SyncZoneOutcome {
        updated: true,
        previous_zone: Some(0),
    })
    .unwrap();
    assert_eq!(v, serde_json::json!({"updated": true, "previousZone": 0}));
    let v = serde_json::to_value(SyncZoneOutcome {
        updated: false,
        previous_zone: None,
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"updated": false, "previousZone": null})
    );
}
