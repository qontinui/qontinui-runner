//! Tests of the one awaiting-restore predicate (plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`,
//! Phase 4). Each names the mutation it was shown red by. Page placement's own
//! tests (`terminal::page_placement`) cover its reading of the same predicate.

use super::*;
use crate::session::close_reason::CloseReason;
use crate::session::session_lifecycle_store::test_open_record;

/// This process booted at this instant in the pure tests.
const BOOT_MS: i64 = 1_000_000;

/// An explicit, classified view: no test depends on the process-wide boot
/// classification, which another test in the binary could latch.
const VIEW: RestoreView = RestoreView {
    prior_marker_at: None,
    boot_was_clean: false,
    booted_at_ms: Some(BOOT_MS),
};

const UNCLASSIFIED: RestoreView = RestoreView {
    booted_at_ms: None,
    ..VIEW
};

fn closed(id: &str, reason: &str, at: i64) -> TerminalSessionRecord {
    TerminalSessionRecord {
        state: "closed".to_string(),
        closed_at: Some(at),
        close_reason: Some(reason.to_string()),
        ..test_open_record(id)
    }
}

fn new_store() -> (tempfile::TempDir, SessionLifecycleStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
    (dir, store)
}

/// The view a test store's rows are judged under: booted at `booted_at_ms`.
fn view_at(booted_at_ms: i64) -> RestoreView {
    RestoreView {
        booted_at_ms: Some(booted_at_ms),
        ..VIEW
    }
}

/// The predicate over single rows: an open row is owed, a row this process
/// closed is not (at the boot instant included), a row closed before the boot
/// still is, a finished row never is, and before the boot is classified an
/// unfinished row cannot be told. Mutations: `closed_since_boot` ignoring
/// `closed_at` (always `Some(false)`); its `>=` made `>`; the finished check
/// in `restore_still_owed` removed.
#[test]
fn the_predicate_reads_each_row_against_this_boot() {
    let open = test_open_record("open");
    assert_eq!(restore_still_owed(&open, &VIEW), Some(true));
    assert_eq!(
        restore_still_owed(&closed("after", "pty-exit", BOOT_MS + 5), &VIEW),
        Some(false),
        "closed by this process"
    );
    assert_eq!(
        restore_still_owed(&closed("at", "pty-exit", BOOT_MS), &VIEW),
        Some(false),
        "a close AT the boot instant is this process's"
    );
    assert_eq!(
        restore_still_owed(&closed("before", "pty-exit", BOOT_MS - 5), &VIEW),
        Some(true),
        "closed by the previous process: the restore brings it back"
    );

    let finished = TerminalSessionRecord {
        finished_at: Some(BOOT_MS - 10),
        ..test_open_record("finished")
    };
    assert_eq!(restore_still_owed(&finished, &VIEW), Some(false));
    assert_eq!(
        restore_still_owed(&finished, &UNCLASSIFIED),
        Some(false),
        "finished is final for restore, classified or not"
    );

    assert_eq!(restore_still_owed(&open, &UNCLASSIFIED), None);
    assert_eq!(
        restore_still_owed(&closed("after", "pty-exit", BOOT_MS + 5), &UNCLASSIFIED),
        None
    );
}

/// Through a real store, as the ledger reads it: an open row on a page nothing
/// has opened yet is owed; rows closed `explicit` or `no-terminal` since the
/// boot are not (both leave the restore set); a `pty-exit` close since the boot
/// stays IN the restore set (for the next boot) and is still not owed; a
/// finished row is not; and an id the store does not hold is not. Mutations:
/// `closed_since_boot` ignoring `closed_at` (the `pty-exit` row reads owed);
/// `OwedRestores::read` inserting every row of the restore set.
#[test]
fn the_live_store_says_which_sessions_are_still_owed() {
    let (_dir, store) = new_store();
    for id in [
        "Awaiting",
        "explicit",
        "no-terminal",
        "pty-exit",
        "finished",
    ] {
        store.record_open(TerminalSessionRecord {
            page_id: "p-not-opened".to_string(),
            ..test_open_record(id)
        });
    }
    store
        .set_finished("finished", true, None)
        .expect("a known session");
    let booted_at_ms = chrono::Utc::now().timestamp_millis();
    store.record_close("explicit", CloseReason::Explicit);
    store.record_close("no-terminal", CloseReason::NoTerminal);
    store.record_close("pty-exit", CloseReason::PtyExit);
    let view = view_at(booted_at_ms);

    let in_set: Vec<String> = view
        .restore_set(&store)
        .into_iter()
        .map(|r| r.claude_session_id)
        .collect();
    assert!(
        in_set.iter().any(|id| id == "pty-exit"),
        "fixture: a pty-exit close stays in the restore set: {in_set:?}"
    );
    assert!(
        !in_set
            .iter()
            .any(|id| id == "explicit" || id == "no-terminal"),
        "fixture: explicit and no-terminal closes leave it: {in_set:?}"
    );

    let owed = OwedRestores::read(Some(&store), &view);
    assert_eq!(owed.of("Awaiting"), Some(true));
    assert_eq!(
        owed.of("awaiting"),
        Some(true),
        "matched case-insensitively"
    );
    for id in [
        "explicit",
        "no-terminal",
        "pty-exit",
        "finished",
        "never-held",
    ] {
        assert_eq!(owed.of(id), Some(false), "{id}");
    }
}

/// With no store, or before the boot is classified, nothing can be answered:
/// `null`, never `false` and never `true`. That holds for an EMPTY restore set
/// too, since it was read under stand-in inputs. Mutations: `OwedRestores::of`
/// reading a missing set as empty (`unwrap_or(false)`); the `booted_at_ms`
/// check in `OwedRestores::read` removed (the empty set then answers `false`).
#[test]
fn with_no_store_or_an_unclassified_boot_the_value_is_null() {
    assert_eq!(OwedRestores::read(None, &VIEW).of("any"), None);

    let (_dir, store) = new_store();
    store.record_open(test_open_record("open"));
    let owed = OwedRestores::read(Some(&store), &UNCLASSIFIED);
    assert_eq!(owed.of("open"), None);
    assert_eq!(owed.of("never-held"), None);

    let (_dir, empty) = new_store();
    assert!(UNCLASSIFIED.restore_set(&empty).is_empty());
    assert_eq!(
        OwedRestores::read(Some(&empty), &UNCLASSIFIED).of("x"),
        None
    );
    assert_eq!(
        OwedRestores::read(Some(&empty), &VIEW).of("x"),
        Some(false),
        "the control: a classified boot answers for an empty set"
    );
}

/// 0, 1 and many owed sessions. Mutation: `OwedRestores::read` keeping only
/// the first owed row (a `break` after the first insert).
#[test]
fn zero_one_and_many_owed_sessions() {
    let (_dir, none_owed) = new_store();
    none_owed.record_open(test_open_record("gone"));
    let booted_at_ms = chrono::Utc::now().timestamp_millis();
    none_owed.record_close("gone", CloseReason::Explicit);
    assert_eq!(
        OwedRestores::read(Some(&none_owed), &view_at(booted_at_ms)).of("gone"),
        Some(false)
    );

    let (_dir, many) = new_store();
    let ids: Vec<String> = (0..5).map(|i| format!("s{i}")).collect();
    for id in &ids {
        many.record_open(test_open_record(id));
    }
    let owed = OwedRestores::read(Some(&many), &view_at(booted_at_ms));
    for id in &ids {
        assert_eq!(owed.of(id), Some(true), "{id}");
    }
    let (_dir, one) = new_store();
    one.record_open(test_open_record("only"));
    assert_eq!(
        OwedRestores::read(Some(&one), &view_at(booted_at_ms)).of("only"),
        Some(true)
    );
}
