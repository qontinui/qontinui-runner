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

    let owed = OwedRestores::read(Some(&store), &view, &PagesRestored::NoneYet);
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
    assert_eq!(
        OwedRestores::read(None, &VIEW, &PagesRestored::NoneYet).of("any"),
        None
    );

    let (_dir, store) = new_store();
    store.record_open(test_open_record("open"));
    let owed = OwedRestores::read(Some(&store), &UNCLASSIFIED, &PagesRestored::NoneYet);
    assert_eq!(owed.of("open"), None);
    assert_eq!(owed.of("never-held"), None);

    let (_dir, empty) = new_store();
    assert!(UNCLASSIFIED.restore_set(&empty).is_empty());
    assert_eq!(
        OwedRestores::read(Some(&empty), &UNCLASSIFIED, &PagesRestored::NoneYet).of("x"),
        None
    );
    assert_eq!(
        OwedRestores::read(Some(&empty), &VIEW, &PagesRestored::NoneYet).of("x"),
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
        OwedRestores::read(
            Some(&none_owed),
            &view_at(booted_at_ms),
            &PagesRestored::NoneYet
        )
        .of("gone"),
        Some(false)
    );

    let (_dir, many) = new_store();
    let ids: Vec<String> = (0..5).map(|i| format!("s{i}")).collect();
    for id in &ids {
        many.record_open(test_open_record(id));
    }
    let owed = OwedRestores::read(Some(&many), &view_at(booted_at_ms), &PagesRestored::NoneYet);
    for id in &ids {
        assert_eq!(owed.of(id), Some(true), "{id}");
    }
    let (_dir, one) = new_store();
    one.record_open(test_open_record("only"));
    assert_eq!(
        OwedRestores::read(Some(&one), &view_at(booted_at_ms), &PagesRestored::NoneYet).of("only"),
        Some(true)
    );
}

fn progress(complete: &[&str], deferred: &[&str], known: &[&str]) -> PagesRestored {
    let set = |pages: &[&str]| pages.iter().map(|p| p.to_string()).collect();
    PagesRestored::Reported(PageRestoreProgress {
        complete: set(complete),
        deferred: set(deferred),
        known: set(known),
    })
}

/// The page rule, as the webview's strip reads it (`pageRestoreSettled`): a
/// page in the layout has run once its own restore completed; a deferred page
/// has not, even when also complete; a page in no layout holds orphans, which
/// have run once any page has; a blank page id is `default`. With no restore
/// set handed out yet nothing has run, and with no report after one it cannot
/// be told. Mutations: the deferred check removed; the orphan arm removed
/// (`complete.contains` alone); the orphan arm counting a deferred page as
/// run, or requiring every completed page to be undeferred (`all`); the
/// blank-to-`default` mapping removed.
#[test]
fn which_pages_restore_has_run() {
    let known = ["default", "p2"];
    let ran = |pages: &PagesRestored, page: &str| pages.has_run(page);

    let one = progress(&["default"], &[], &known);
    assert_eq!(ran(&one, "default"), Some(true));
    assert_eq!(ran(&one, ""), Some(true), "blank is default");
    assert_eq!(ran(&one, "p2"), Some(false), "its own restore has not run");
    assert_eq!(
        ran(&one, "orphan-page"),
        Some(true),
        "adopted by the first page"
    );

    let deferred = progress(&["default"], &["default"], &known);
    assert_eq!(ran(&deferred, "default"), Some(false), "deferred re-runs");
    let one_of_two = progress(&["default", "p2"], &["default"], &known);
    assert_eq!(
        ran(&one_of_two, "orphan-page"),
        Some(true),
        "p2 ran, so its first restore adopted the orphans"
    );
    assert_eq!(
        ran(&deferred, "orphan-page"),
        Some(false),
        "a deferred page adopted no orphans"
    );

    let none = progress(&[], &[], &known);
    assert_eq!(
        ran(&none, "orphan-page"),
        Some(false),
        "no page has run yet"
    );

    assert_eq!(ran(&PagesRestored::NoneYet, "default"), Some(false));
    assert_eq!(ran(&PagesRestored::Unknown, "default"), None);
}

/// Through a real store: an owed row on a page whose restore already ran is
/// not owed (that restore declined it); one on a page still to restore is;
/// one whose page cannot be told is `null`; a row the predicate already drops
/// stays `false` whatever the pages say. Mutation: `OwedRestores::read`
/// ignoring `pages` (every predicate-owed row reads `true`).
#[test]
fn a_row_whose_page_already_restored_is_not_owed() {
    let (_dir, store) = new_store();
    for (id, page) in [("declined", "ran"), ("waiting", "not-yet"), ("gone", "ran")] {
        store.record_open(TerminalSessionRecord {
            page_id: page.to_string(),
            ..test_open_record(id)
        });
    }
    let booted_at_ms = chrono::Utc::now().timestamp_millis();
    store.record_close("gone", CloseReason::PtyExit);
    let view = view_at(booted_at_ms);

    let owed = OwedRestores::read(
        Some(&store),
        &view,
        &progress(&["ran"], &[], &["ran", "not-yet"]),
    );
    assert_eq!(owed.of("declined"), Some(false));
    assert_eq!(owed.of("waiting"), Some(true), "the control");
    assert_eq!(owed.of("gone"), Some(false));
    assert_eq!(owed.of("never-held"), Some(false));

    let unknown = OwedRestores::read(Some(&store), &view, &PagesRestored::Unknown);
    assert_eq!(unknown.of("declined"), None);
    assert_eq!(
        unknown.of("gone"),
        Some(false),
        "the predicate already answers"
    );
}

fn report(complete: &[&str], deferred: &[&str]) -> PageRestoreProgress {
    let set = |pages: &[&str]| pages.iter().map(|p| p.to_string()).collect();
    PageRestoreProgress {
        complete: set(complete),
        deferred: set(deferred),
        known: set(&["default", "p2", "p3"]),
    }
}

/// The report-vs-latch matrix and the multi-window fold: any window's report
/// wins over the latch; with none, a latch that has not handed out a restore
/// set (not yet run, or withheld) means no page has run, and one that has
/// means it cannot be told. Two windows' reports are merged, so a page either
/// window completed has run. Mutations: the fold keeping only the last window;
/// the `NoneYet` and `Unknown` arms swapped; the latch read before the reports.
#[test]
fn pages_restored_merges_the_windows_and_falls_back_to_the_latch() {
    use BootRestorePhase::{NotYetRun, Ran, Withheld};
    let none: Vec<PageRestoreProgress> = Vec::new();
    assert_eq!(
        PagesRestored::from_parts(none.iter(), NotYetRun),
        PagesRestored::NoneYet
    );
    assert_eq!(
        PagesRestored::from_parts(none.iter(), Withheld),
        PagesRestored::NoneYet
    );
    assert_eq!(
        PagesRestored::from_parts(none.iter(), Ran),
        PagesRestored::Unknown
    );

    let two = [report(&["default"], &[]), report(&["p2"], &[])];
    for phase in [NotYetRun, Withheld, Ran] {
        let pages = PagesRestored::from_parts(two.iter(), phase);
        assert_eq!(pages.has_run("default"), Some(true), "{phase:?}");
        assert_eq!(pages.has_run("p2"), Some(true), "{phase:?}");
    }
}

/// A closed window keeps the restores it ran and drops its deferrals: window A
/// completes p2 and closes, and p2 still reads run; a page A had deferred (it
/// is in A's `complete` too) neither pins nor counts once A closes; another
/// window's newer report replaces its older one; an open window's unreadable
/// report makes the whole answer Unknown, until that window closes. Mutations:
/// `forget` dropping the completions; `forget` keeping the deferrals; `forget`
/// retaining a deferred page's completion; `record` keeping the first report;
/// `pages` ignoring an unreadable report.
#[test]
fn a_closed_window_keeps_its_restores_and_drops_its_deferrals() {
    use BootRestorePhase::Ran;
    let mut reports = WindowReports::default();
    reports.record("main", Some(report(&["default"], &[])));
    reports.record("term-a", Some(report(&["p2", "p3"], &["p3"])));
    assert_eq!(reports.pages(Ran).has_run("p3"), Some(false), "deferred");

    reports.forget("term-a");
    let pages = reports.pages(Ran);
    assert_eq!(
        pages.has_run("p2"),
        Some(true),
        "A's restore of p2 still ran"
    );
    assert_eq!(
        pages.has_run("p3"),
        Some(false),
        "A's deferral of p3 no longer pins it, and it never restored"
    );
    reports.record("main", Some(report(&["default", "p3"], &[])));
    assert_eq!(reports.pages(Ran).has_run("p3"), Some(true));
    reports.record("main", Some(report(&["default"], &[])));
    assert_eq!(
        reports.pages(Ran).has_run("p3"),
        Some(false),
        "main's newer report replaced its older one"
    );
    assert_eq!(reports.pages(Ran).has_run("p2"), Some(true));

    reports.record("term-b", None);
    assert_eq!(reports.pages(Ran), PagesRestored::Unknown);
    reports.forget("term-b");
    assert_eq!(reports.pages(Ran).has_run("p2"), Some(true));
}

/// A closing window's webview outlives the forget until its asynchronous
/// `destroy()` lands, and a report it sends in between is refused: it cannot
/// re-pin the deferral the forget dropped. A window built again under the
/// same label reports normally. Mutations: `record` ignoring the tombstone;
/// `forget` not tombstoning; `reopen` a no-op.
#[test]
fn a_closed_windows_late_report_is_refused_until_the_label_is_reopened() {
    use BootRestorePhase::Ran;
    let mut reports = WindowReports::default();
    reports.record("main", Some(report(&["default"], &[])));
    reports.record("term-a", Some(report(&["p2"], &[])));
    reports.forget("term-a");

    reports.record("term-a", Some(report(&["p2"], &["p2"])));
    assert_eq!(
        reports.pages(Ran).has_run("p2"),
        Some(true),
        "the late report after the forget is refused"
    );

    reports.reopen("term-a");
    reports.record("term-a", Some(report(&["p2"], &["p2"])));
    assert_eq!(
        reports.pages(Ran).has_run("p2"),
        Some(false),
        "a rebuilt term-a's deferral counts"
    );
}

/// With no open window reporting, a closed window's retained completions do
/// not decide: they carry no layout, so on their own every layout page would
/// read as an orphan the first restore adopted. The latch decides instead
/// (Unknown once a restore set was handed out, nothing run before). Mutation:
/// `pages` chaining the closed set while `live` is empty.
#[test]
fn closed_completions_alone_do_not_make_layout_pages_read_restored() {
    use BootRestorePhase::{NotYetRun, Ran};
    let mut reports = WindowReports::default();
    reports.record("term-a", Some(report(&["p2"], &[])));
    reports.forget("term-a");

    assert_eq!(reports.pages(Ran), PagesRestored::Unknown);
    assert_eq!(reports.pages(Ran).has_run("default"), None);
    assert_eq!(reports.pages(NotYetRun).has_run("default"), Some(false));

    reports.record("main", Some(report(&["default"], &[])));
    assert_eq!(
        reports.pages(Ran).has_run("p2"),
        Some(true),
        "with an open window reporting, the closed completion adds"
    );
}

/// A closed window's `known` set is not kept: the open windows report the
/// whole layout, so a page only the closed window listed is no longer a layout
/// page, and its rows are orphans the first restore adopted. Mutation: `forget`
/// retaining `known` (the page then reads as a layout page that never ran).
#[test]
fn a_closed_windows_known_pages_are_not_kept() {
    use BootRestorePhase::Ran;
    let progress = |complete: &[&str], known: &[&str]| PageRestoreProgress {
        complete: complete.iter().map(|p| p.to_string()).collect(),
        deferred: HashSet::new(),
        known: known.iter().map(|p| p.to_string()).collect(),
    };
    let mut reports = WindowReports::default();
    reports.record("main", Some(progress(&["default"], &["default"])));
    reports.record("term-a", Some(progress(&[], &["default", "gone-page"])));
    assert_eq!(reports.pages(Ran).has_run("gone-page"), Some(false));

    reports.forget("term-a");
    assert_eq!(
        reports.pages(Ran).has_run("gone-page"),
        Some(true),
        "no open window lists it: an orphan, adopted by main's restore"
    );
}

/// A window's report is bounded: ids are trimmed and blank ones dropped, and
/// a list over the cap, or an id over the length cap, makes the report
/// unreadable rather than silently orphaning the pages past the cap.
/// Mutations: the trim removed; the count cap applied by truncation, or
/// before de-duplication; the length check removed.
#[test]
fn a_report_is_trimmed_and_an_oversized_one_is_unreadable() {
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let read = PageRestoreProgress::from_report(ids(&[" p1 ", ""]), ids(&[]), ids(&["p1", "  "]))
        .expect("a small report reads");
    assert_eq!(read.complete, ["p1".to_string()].into_iter().collect());
    assert_eq!(read.known, ["p1".to_string()].into_iter().collect());

    let many: Vec<String> = (0..=MAX_REPORTED_PAGES).map(|i| format!("p{i}")).collect();
    assert_eq!(PageRestoreProgress::from_report(vec![], vec![], many), None);
    let at_cap: Vec<String> = (0..MAX_REPORTED_PAGES).map(|i| format!("p{i}")).collect();
    assert!(PageRestoreProgress::from_report(vec![], vec![], at_cap).is_some());
    let repeated: Vec<String> = (0..=MAX_REPORTED_PAGES).map(|_| "p".to_string()).collect();
    assert!(
        PageRestoreProgress::from_report(vec![], vec![], repeated).is_some(),
        "the cap counts distinct ids, as stored"
    );
    let long = "x".repeat(MAX_PAGE_ID_LEN + 1);
    assert_eq!(
        PageRestoreProgress::from_report(vec![long], vec![], vec![]),
        None
    );
}

/// Serializes the tests that touch the process-global window reports.
static REGISTRY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The process-global wiring the ledger reads: two windows' reports are both
/// read through [`PagesRestored::current`], and a closed window's deferral
/// stops pinning while its restore still counts. Mutation:
/// `forget_page_restore_progress` a no-op.
#[test]
fn the_global_registry_reads_every_window_and_forgets_a_closed_ones_deferrals() {
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_page_restore_progress_for_test();

    record_page_restore_progress("main", Some(report(&["default"], &[])));
    record_page_restore_progress("term-1", Some(report(&["p2", "p3"], &["p3"])));
    let pages = PagesRestored::current();
    assert_eq!(
        pages.has_run("default"),
        Some(true),
        "main's report is read"
    );
    assert_eq!(pages.has_run("p2"), Some(true), "term-1's report is read");
    assert_eq!(pages.has_run("p3"), Some(false), "term-1 deferred it");

    forget_page_restore_progress("term-1");
    let pages = PagesRestored::current();
    assert_eq!(pages.has_run("p2"), Some(true), "its restore still ran");
    assert_eq!(
        pages.has_run("p3"),
        Some(false),
        "never restored, and no longer pinned by a deferral"
    );

    reset_page_restore_progress_for_test();
}
