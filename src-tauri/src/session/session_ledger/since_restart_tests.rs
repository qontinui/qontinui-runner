//! The "Since restart" accounting of plan
//! `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`: Phase 0
//! pinned its defects as tests RED on the code they were written against
//! (`since_restart_*`, the first five); Phases 2 and 3 turned them green, and
//! the Phase 3 tests below them pin the classification that did it.
//!
//! Every test enters the way production does:
//!
//! - a real [`SessionLifecycleStore`] in a tempdir;
//! - the previous process's roster captured by the production [`capture`];
//! - the liveness poll's ticks ([`Restart::poll_tick`]): the aftermath record
//!   ([`crate::session::ledger_aftermath`]) fed from the registry after the
//!   ledger capture, then with the rows the classify loop kept alive on their
//!   own pane (the `touch` its `KeepAlive` writes) and the tick's live-session
//!   read — the two points `main.rs`'s lifecycle poll feeds it;
//! - closes through the production [`SessionLifecycleStore::record_close`] with
//!   the reasons production writes (`explicit`, `pty-exit`, `no-terminal`);
//! - the classification through [`observed_back`] + [`registry_now`] + the
//!   record's inputs + [`diff`] / [`generation_reports`] — the sequence
//!   [`report`] runs. `report` itself is not callable here: it reads the
//!   process-global ledger generations, the latched boot census and the
//!   process's own record.
//!
//! Each assertion states the INTENDED behaviour (the plan's D1, D2 and D4),
//! and each test asserts a control first: a session nothing brought back stays
//! `missing`, so a fix cannot pass by reporting nothing missing at all.

use super::*;
use crate::session::claude_session_registry::read_live_sessions;
use crate::session::close_reason::CloseReason;
use crate::session::ledger_aftermath::{LedgerAftermath, ProcessRead};
use crate::session::session_lifecycle_store::{PollTerminalMatch, TerminalSessionRecord};

/// The generation file the restart under test rotated its prior roster into.
const GENERATION_FILE: &str = "session-ledger.1000.json";

/// A pane alive after the restart that hosts nothing under test, so the
/// live-terminal input is a real, non-empty set rather than an empty one.
const UNRELATED_LIVE_PANE: &str = "term-unrelated";

/// Comfortably past the store's closed-row retention (24 h,
/// `CLOSED_RETENTION_MS`, private to the store): `prune` drops a closed row
/// whose terminal is gone once it closed more than that long ago.
const TWO_DAYS_MS: i64 = 2 * 86_400_000;

#[derive(Debug)]
struct NoTranscripts;

impl TranscriptProbe for NoTranscripts {
    fn transcript_exists(&self, _session_id: &str, _working_dir: Option<&str>) -> bool {
        false
    }
}

/// The first wall-clock millisecond strictly after `ms`.
///
/// The store stamps `last_seen_at` and `closed_at` from the wall clock, so a
/// test that needs "before the boot" and "after the boot" to be distinct
/// instants waits the millisecond out rather than hoping two calls land in
/// different ones.
fn first_ms_after(ms: i64) -> i64 {
    loop {
        let now = chrono::Utc::now().timestamp_millis();
        if now > ms {
            return now;
        }
        std::thread::yield_now();
    }
}

/// A session row as the previous process recorded it: confirmed, with a known
/// account, on `terminal_id`.
fn prior_row(id: &str, terminal_id: &str) -> TerminalSessionRecord {
    TerminalSessionRecord {
        claude_session_id: id.to_string(),
        config_dir: Some("C:/claude/.claude-ben".to_string()),
        working_dir: Some("C:/repo".to_string()),
        page_id: "default".to_string(),
        zone_index: 0,
        title: Some("claude".to_string()),
        terminal_id: terminal_id.to_string(),
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

/// One restart, reached the way production reaches it: the previous process had
/// every `prior_ids` session open on its own pane (`term-<id>`), its liveness
/// poll captured them into the roster, and then the runner restarted.
struct Restart {
    dir: tempfile::TempDir,
    store: SessionLifecycleStore,
    /// The roster the previous process captured: this boot's generation.
    prior: SessionLedger,
    /// This process's boot instant: after the capture, before anything a test
    /// does next.
    boot_at_ms: i64,
    /// This boot's aftermath record, fed where the liveness poll feeds it.
    aftermath: LedgerAftermath,
}

impl Restart {
    fn new(prior_ids: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("terminal-sessions.json")).unwrap();
        for id in prior_ids {
            store.record_open(prior_row(id, &format!("term-{id}")));
        }
        let captured_at_ms = chrono::Utc::now().timestamp_millis();
        let prior = capture(&store, &NoTranscripts, REASON_POLL, captured_at_ms, None);
        let mut on_roster: Vec<&str> = prior
            .sessions
            .iter()
            .map(|e| e.claude_session_id.as_str())
            .collect();
        on_roster.sort_unstable();
        let mut expected = prior_ids.to_vec();
        expected.sort_unstable();
        assert_eq!(
            on_roster, expected,
            "fixture: every prior session is on the captured roster"
        );
        let boot_at_ms = first_ms_after(captured_at_ms);
        // This boot retained the prior roster into GENERATION_FILE and opened
        // its aftermath record beside it, as `open_for_this_boot` does.
        let retained: HashSet<String> = [GENERATION_FILE.to_string()].into_iter().collect();
        let aftermath = LedgerAftermath::open(
            dir.path(),
            boot_at_ms,
            Some(GENERATION_FILE.to_string()),
            Some(&prior),
            &retained,
        );
        // Everything a test does next happens strictly after the boot.
        first_ms_after(boot_at_ms);
        Self {
            dir,
            store,
            prior,
            boot_at_ms,
            aftermath,
        }
    }

    /// One liveness-poll tick, as `main.rs` feeds the aftermath record: (a) the
    /// registry right after the ledger capture, then (b) the rows the classify
    /// loop kept alive on their own pane and the tick's live-session read.
    fn poll_tick(&self, kept_alive: &[&str], process: ProcessRead) {
        self.aftermath.observe_registry(&self.store.all_records());
        let kept: Vec<(String, PollTerminalMatch)> = kept_alive
            .iter()
            .map(|id| (id.to_string(), PollTerminalMatch::Direct))
            .collect();
        self.aftermath
            .observe_tick(chrono::Utc::now().timestamp_millis(), &kept, process);
    }

    /// The liveness poll found `id` alive this boot: its row matched its own
    /// live pane and was `KeepAlive`d — the `touch` the poll writes, in a tick
    /// that feeds the aftermath record the same observation.
    fn poll_saw_alive(&self, id: &str) {
        self.store.touch(id);
        self.poll_tick(&[id], ProcessRead::Held(Vec::new()));
    }

    /// `GET /control/sessions/ledger` as [`report`] builds it, after one more
    /// poll tick whose live-session read found `held` held by a live `claude`;
    /// `live_panes` are the PTYs alive in this process now. The boot census is
    /// latched and the terminal manager answers, so a miss count here is a
    /// claim, not an upper bound (`restore_stamps_available` is true, as in
    /// `report`).
    fn report(&self, live_panes: &[&str], held: &[String]) -> LedgerReport {
        self.report_after(live_panes, ProcessRead::Held(held.to_vec()))
    }

    /// The boot facts this restart's restore set is read under: classified,
    /// booted at `boot_at_ms`, and a crash recovery (no shutdown marker).
    fn restore_view(&self) -> RestoreView {
        RestoreView {
            prior_marker_at: None,
            boot_was_clean: false,
            booted_at_ms: Some(self.boot_at_ms),
        }
    }

    /// [`Self::report`] after a poll tick whose live-session read was
    /// `process`.
    fn report_after(&self, live_panes: &[&str], process: ProcessRead) -> LedgerReport {
        self.report_under(live_panes, process, &self.restore_view())
    }

    /// [`Self::report_after`] with `restoreOwed` read under `view`, as
    /// [`report`] reads it under [`RestoreView::current`].
    fn report_under(
        &self,
        live_panes: &[&str],
        process: ProcessRead,
        view: &RestoreView,
    ) -> LedgerReport {
        self.poll_tick(&[], process);
        let live: HashSet<String> = live_panes.iter().map(|t| t.to_string()).collect();
        let back = observed_back(&self.store, Some(self.boot_at_ms), Some(&live));
        let now_state = AfterRestart {
            registry: registry_now(&self.store),
            aftermath: self.aftermath.inputs(chrono::Utc::now().timestamp_millis()),
            restore_owed: OwedRestores::read(Some(&self.store), view),
        };
        let current = capture(
            &self.store,
            &NoTranscripts,
            "read",
            chrono::Utc::now().timestamp_millis(),
            None,
        );
        let mut report = diff(Some(&self.prior), &back, &now_state, current, true);
        let generation = StoredGeneration {
            path: PathBuf::from(GENERATION_FILE),
            rotated_at_ms: self.boot_at_ms,
            ledger: self.prior.clone(),
        };
        report.generations = generation_reports(
            &[generation],
            Some(Path::new(GENERATION_FILE)),
            &back,
            &now_state,
            true,
        );
        report
    }
}

/// Where `id` landed in one set of buckets. `closed` is the deliberate-close
/// bucket, the plan's D2 class (`closed-since`).
fn bucket_in(
    returned: &[LedgerOutcome],
    missing: &[LedgerOutcome],
    finished: &[LedgerOutcome],
    closed: &[LedgerOutcome],
    id: &str,
) -> &'static str {
    let has = |v: &[LedgerOutcome]| v.iter().any(|o| o.claude_session_id == id);
    match (has(returned), has(missing), has(finished), has(closed)) {
        (true, false, false, false) => "back",
        (false, true, false, false) => "missing",
        (false, false, true, false) => "finished",
        (false, false, false, true) => "closed-since",
        (false, false, false, false) => "absent",
        _ => "several",
    }
}

/// `id`'s bucket in both places the report serves it, as `(top-level, this
/// boot's generation)`. The top-level `missing[]` is what `/resume-unfinished`
/// takes as its runner-mode candidates; the generation is what the strip and
/// the panel read.
fn buckets(report: &LedgerReport, id: &str) -> (&'static str, &'static str) {
    let generation = &report.generations[0];
    assert!(
        generation.this_boot,
        "fixture: the generation under test is this boot's"
    );
    (
        bucket_in(
            &report.returned,
            &report.missing,
            &report.finished,
            &report.closed_since,
            id,
        ),
        bucket_in(
            &generation.returned,
            &generation.missing,
            &generation.finished,
            &generation.closed_since,
            id,
        ),
    )
}

/// Defect 1 ("Why" 1; Reading 1's 36 `explicit` closes). A session the poll saw
/// alive after the restart, then closed by the webview's tab close (`explicit`),
/// DID come back. `observed_back` counts only a row still open on a live pane or
/// stamped by this boot's restore, so the close flips it to `missing`.
/// Intended (D1, D4): back, carrying a later close.
#[test]
fn since_restart_a_session_seen_alive_then_closed_explicit_is_back_not_missing() {
    let restart = Restart::new(&["came-back", "lost"]);
    restart.poll_saw_alive("came-back");
    restart
        .store
        .record_close("came-back", CloseReason::Explicit);

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    assert_eq!(
        buckets(&report, "came-back"),
        ("back", "back"),
        "a session the poll saw alive after the restart and then closed `explicit` came \
         back: it must read back (with a later close), never missing"
    );
}

/// Defect 1 for the other production close reasons: a `pty-exit` (a crash, or
/// `DELETE /terminals/{id}`, finding `9d26f909`) and a poll `no-terminal`
/// (Reading 1's 6). A close cannot undo a return, whatever its reason.
#[test]
fn since_restart_a_session_seen_alive_then_closed_pty_exit_or_no_terminal_is_back() {
    let closes = [
        ("exited", CloseReason::PtyExit),
        ("orphaned", CloseReason::NoTerminal),
    ];
    let restart = Restart::new(&["exited", "orphaned", "lost"]);
    for (id, reason) in closes {
        restart.poll_saw_alive(id);
        restart.store.record_close(id, reason);
    }

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    let not_back: Vec<(&str, &str, (&str, &str))> = closes
        .iter()
        .map(|(id, reason)| (*id, reason.as_str(), buckets(&report, id)))
        .filter(|(_, _, placed)| *placed != ("back", "back"))
        .collect();
    assert!(
        not_back.is_empty(),
        "a session the poll saw alive after the restart came back, whatever later closed \
         it; these read otherwise as (id, close reason, (top-level, this boot's \
         generation)): {not_back:?}"
    );
}

/// Defect 1, Reading 2's shape (finding `f0a0c310`): the usage-limit migration
/// respawned the session into a new pane, and a stale bind left its row `open`
/// on the EXITED source pane, while a live `claude` holds its id. The process
/// evidence is Claude Code's own `sessions/<pid>.json` for a pid alive in the
/// process table, read through `read_live_sessions`, the reader the poll builds
/// `live_identity` from on every snapshot tick. Intended (D1): back, on that
/// evidence, whatever terminal the row points at.
///
/// The fixture below is the real reader's real input, asserted to read as
/// live; it is handed to the poll tick [`Restart::report`] runs, which records
/// it in the aftermath record (`process`) and as the newest tick's process
/// evidence — the two arms `classify` reads besides "back now".
#[test]
fn since_restart_a_row_on_an_exited_pane_whose_id_a_live_claude_holds_is_back() {
    let restart = Restart::new(&["migrated", "lost"]);
    // The stale bind: the row re-pointed at the source pane, which then exited.
    restart
        .store
        .record_open(prior_row("migrated", "term-exited-source"));
    // Claude Code's registry: pid 4242 runs `claude --resume migrated` in the
    // NEW pane, under the account the migration moved it to.
    let config_dir = restart.dir.path().join(".claude-ben");
    std::fs::create_dir_all(config_dir.join("sessions")).unwrap();
    std::fs::write(
        config_dir.join("sessions").join("4242.json"),
        r#"{"pid":4242,"sessionId":"migrated","cwd":"C:/repo","kind":"interactive","name":"name-migrated","status":"busy"}"#,
    )
    .unwrap();
    let live_pids: HashSet<u32> = [4242_u32].into_iter().collect();
    let read = read_live_sessions(&[config_dir], &live_pids, &|_| true);
    assert!(read.is_complete(), "fixture: every record was read");
    let held: Vec<String> = read.sessions.into_iter().map(|s| s.session_id).collect();
    assert_eq!(
        held,
        vec!["migrated".to_string()],
        "fixture: the production registry reader reads the session as held by a live claude"
    );

    let report = restart.report(&["term-new-pane", UNRELATED_LIVE_PANE], &held);

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    assert_eq!(
        buckets(&report, "migrated"),
        ("back", "back"),
        "a live claude holds this session's id (`sessions/4242.json`, pid live): it came \
         back, though its row points at an exited pane"
    );
}

/// Defect 2 ("Why" 2): the closed-on-purpose class never fired. A session that
/// never came back and was then closed `explicit` (the webview's tab close) read
/// `missing`, because the overlay filed a session as closed on purpose only for
/// `user-close`, which no production code writes. Intended (D2): not missing,
/// in the deliberate-close bucket (`closed-since`), by the typed close-reason
/// set's classification of `explicit`.
#[test]
fn since_restart_a_session_closed_explicit_without_coming_back_is_closed_since_not_missing() {
    let restart = Restart::new(&["closed-on-purpose", "lost"]);
    restart
        .store
        .record_close("closed-on-purpose", CloseReason::Explicit);

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);
    let close = registry_now(&restart.store)
        .closed
        .get("closed-on-purpose")
        .cloned();

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    assert_eq!(
        buckets(&report, "closed-on-purpose"),
        ("closed-since", "closed-since"),
        "a session closed `explicit` after the capture without coming back is closed on \
         purpose, never missing (the registry's close of it: {close:?})"
    );
}

/// Defect 3 ("Why" 3; Reading 1's 43rd session): every overlay reads the
/// registry, and the registry prunes a closed row 24 h after it closes. After
/// that prune nothing left says the session in test 1 came back. Intended
/// (D1): it stays back, because what came back is recorded apart from the row.
#[test]
fn since_restart_a_session_that_came_back_stays_back_after_its_closed_row_is_pruned() {
    let restart = Restart::new(&["came-back", "lost"]);
    restart.poll_saw_alive("came-back");
    restart
        .store
        .record_close("came-back", CloseReason::Explicit);
    restart.store.prune(
        chrono::Utc::now().timestamp_millis() + TWO_DAYS_MS,
        &HashSet::new(),
    );
    assert!(
        restart.store.get("came-back").is_none(),
        "fixture: the prune dropped the closed row"
    );
    assert!(
        restart.store.get("lost").is_some(),
        "fixture: the same prune keeps an open row seen two days ago"
    );

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    assert_eq!(
        buckets(&report, "came-back"),
        ("back", "back"),
        "a session that came back stays back after the registry prunes its closed row"
    );
}

// ---------------------------------------------------------------------------
// Phase 3: the classification reads what was observed
// ---------------------------------------------------------------------------

/// The ids in one bucket, in roster order.
fn ids(outcomes: &[LedgerOutcome]) -> Vec<&str> {
    outcomes
        .iter()
        .map(|o| o.claude_session_id.as_str())
        .collect()
}

/// A process read that failed leaves a session the read might have found
/// `missing`, never back, and makes the verdict `unknown` — on the top-level
/// report and on this boot's generation. Control: the same read succeeding
/// without the id is a confident `partial`.
#[test]
fn since_restart_a_failed_held_id_read_leaves_the_session_missing_and_the_verdict_unknown() {
    let restart = Restart::new(&["migrated", "kept"]);
    restart
        .store
        .record_open(prior_row("migrated", "term-exited-source"));
    restart.poll_saw_alive("kept");

    let read_ok = restart.report(&["term-kept", UNRELATED_LIVE_PANE], &[]);
    assert_eq!(buckets(&read_ok, "migrated"), ("missing", "missing"));
    assert_eq!(
        read_ok.verdict, VERDICT_PARTIAL,
        "control: a read that worked"
    );
    assert!(read_ok.unknown_reasons.is_empty());

    let failed = restart.report_after(&["term-kept", UNRELATED_LIVE_PANE], ProcessRead::Failed);
    assert_eq!(
        buckets(&failed, "migrated"),
        ("missing", "missing"),
        "a failed read is never evidence that it came back"
    );
    assert_eq!(buckets(&failed, "kept"), ("back", "back"));
    for (verdict, reasons) in [
        (&failed.verdict, &failed.unknown_reasons),
        (
            &failed.generations[0].verdict,
            &failed.generations[0].unknown_reasons,
        ),
    ] {
        assert_eq!(verdict, VERDICT_UNKNOWN);
        assert_eq!(
            reasons,
            &vec![UNKNOWN_PROCESS_CENSUS_UNAVAILABLE.to_string()]
        );
    }
    assert_eq!(
        failed.reason.as_deref(),
        Some(UNKNOWN_PROCESS_CENSUS_UNAVAILABLE)
    );
    assert!(failed.note.contains("UPPER BOUND"), "{}", failed.note);
}

/// D4: a session that came back and was then closed stays `back` and carries
/// `laterClosed` — deliberate for `explicit`, unknown (`null`) for any other
/// reason — while `closedSince` holds only the session closed on purpose that
/// never came back, and a non-deliberate close of one that never came back is
/// `missing` with its close named.
#[test]
fn since_restart_a_back_session_that_later_closed_carries_later_closed_and_closed_since_holds_only_sessions_that_never_came_back(
) {
    let restart = Restart::new(&[
        "back-explicit",
        "back-pty",
        "closed-on-purpose",
        "orphaned",
        "lost",
    ]);
    restart.poll_saw_alive("back-explicit");
    restart.poll_saw_alive("back-pty");
    restart
        .store
        .record_close("back-explicit", CloseReason::Explicit);
    restart.store.record_close("back-pty", CloseReason::PtyExit);
    restart
        .store
        .record_close("closed-on-purpose", CloseReason::Explicit);
    restart
        .store
        .record_close("orphaned", CloseReason::NoTerminal);

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);

    for (returned, closed_since, missing) in [
        (&report.returned, &report.closed_since, &report.missing),
        (
            &report.generations[0].returned,
            &report.generations[0].closed_since,
            &report.generations[0].missing,
        ),
    ] {
        assert_eq!(ids(returned), vec!["back-explicit", "back-pty"]);
        assert_eq!(ids(closed_since), vec!["closed-on-purpose"]);
        assert_eq!(ids(missing), vec!["lost", "orphaned"]);
        let later = |id: &str| {
            returned
                .iter()
                .find(|o| o.claude_session_id == id)
                .and_then(|o| o.later_closed.clone())
        };
        let explicit = later("back-explicit").expect("a later close");
        assert_eq!(explicit.reason, "explicit");
        assert_eq!(explicit.deliberate, Some(true));
        assert_eq!(
            explicit.at_ms,
            restart.store.get("back-explicit").unwrap().closed_at
        );
        let pty = later("back-pty").expect("a later close");
        assert_eq!(pty.reason, "pty-exit");
        assert_eq!(
            pty.deliberate, None,
            "a pty-exit is not known to be deliberate, nor known not to be"
        );
        let closed = &closed_since[0];
        assert_eq!(closed.outcome, OUTCOME_CLOSED_SINCE);
        assert_eq!(closed.reason.as_deref(), Some("closed:explicit"));
        assert_eq!(closed.later_closed, None);
        let orphaned = missing
            .iter()
            .find(|o| o.claude_session_id == "orphaned")
            .unwrap();
        assert_eq!(orphaned.reason.as_deref(), Some("closed:no-terminal"));
    }
}

/// The amendment of 2026-11-12: a session that came back, was finished and
/// then closed still reads finished after the registry prunes its row (from
/// the aftermath record, which `/resume-unfinished` relies on to skip it).
#[test]
fn since_restart_a_session_that_came_back_finished_and_closed_still_reads_finished_after_the_prune()
{
    let restart = Restart::new(&["done", "lost"]);
    restart.poll_saw_alive("done");
    restart
        .store
        .set_finished("done", true, None)
        .expect("a known session");
    restart.poll_tick(&[], ProcessRead::Held(Vec::new()));
    restart.store.record_close("done", CloseReason::PtyExit);
    restart.poll_tick(&[], ProcessRead::Held(Vec::new()));
    restart.store.prune(
        chrono::Utc::now().timestamp_millis() + TWO_DAYS_MS,
        &HashSet::new(),
    );
    assert!(restart.store.get("done").is_none(), "fixture: pruned");

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);

    assert_eq!(buckets(&report, "done"), ("back", "back"));
    let done = report
        .returned
        .iter()
        .find(|o| o.claude_session_id == "done")
        .unwrap();
    assert!(
        done.finished,
        "finished as recorded, though the row is gone"
    );
    assert_eq!(
        done.later_closed.as_ref().map(|c| c.reason.as_str()),
        Some("pty-exit")
    );
}

/// A session whose row is gone and that no record has is UNKNOWN: `missing`
/// with reason `record-gone`, and the verdict `unknown` (counted as "up to"),
/// never a confident miss.
#[test]
fn since_restart_a_pruned_session_no_record_has_is_record_gone_and_the_verdict_unknown() {
    let restart = Restart::new(&["gone", "lost"]);
    restart.store.record_close("gone", CloseReason::PtyExit);
    restart.store.prune(
        chrono::Utc::now().timestamp_millis() + TWO_DAYS_MS,
        &HashSet::new(),
    );
    assert!(restart.store.get("gone").is_none(), "fixture: pruned");

    let report = restart.report(&[UNRELATED_LIVE_PANE], &[]);

    let reason = |id: &str| {
        report
            .missing
            .iter()
            .find(|o| o.claude_session_id == id)
            .and_then(|o| o.reason.clone())
    };
    assert_eq!(reason("gone").as_deref(), Some(MISSING_RECORD_GONE));
    assert_ne!(
        reason("lost").as_deref(),
        Some(MISSING_RECORD_GONE),
        "control: a row the registry still holds is not record-gone"
    );
    assert_eq!(report.verdict, VERDICT_UNKNOWN);
    assert_eq!(
        report.unknown_reasons,
        vec![UNKNOWN_RECORD_GONE.to_string()]
    );
    assert_eq!(report.generations[0].verdict, VERDICT_UNKNOWN);
}

/// The note's numbers are the buckets' sizes, and its split of the misses is
/// their `restoreOwed` (2 owed and 1 lost, so the two cannot be swapped
/// unseen). Mutation: the note's owed and lost counts swapped.
#[test]
fn since_restart_the_notes_numbers_equal_the_buckets() {
    let restart = Restart::new(&[
        "back",
        "back-closed",
        "missed",
        "missed-too",
        "lost",
        "finished",
        "closed",
    ]);
    restart.poll_saw_alive("back");
    restart.poll_saw_alive("back-closed");
    restart
        .store
        .record_close("back-closed", CloseReason::Explicit);
    restart
        .store
        .set_finished("finished", true, None)
        .expect("a known session");
    restart.store.record_close("closed", CloseReason::Explicit);
    restart.store.record_close("lost", CloseReason::NoTerminal);

    let report = restart.report(&["term-back", UNRELATED_LIVE_PANE], &[]);

    let later = report
        .returned
        .iter()
        .filter(|o| o.later_closed.is_some())
        .count();
    let (back, missing, finished, closed) = (
        report.returned.len(),
        report.missing.len(),
        report.finished.len(),
        report.closed_since.len(),
    );
    let owed_count = |owed: Option<bool>| {
        report
            .missing
            .iter()
            .filter(|m| m.restore_owed == owed)
            .count()
    };
    let (owed, lost) = (owed_count(Some(true)), owed_count(Some(false)));
    assert_eq!(
        (back, later, missing, owed, lost, finished, closed),
        (2, 1, 3, 2, 1, 1, 1)
    );
    let headline = format!(
        "{back} of {} sessions open before the last shutdown came back ({later} later closed); \
         {missing} did not — {owed} still owed a restore by this boot, {lost} will not be \
         restored by it;",
        back + missing
    );
    assert!(report.note.starts_with(&headline), "{}", report.note);
    assert!(
        !report.note.contains("could not be read"),
        "every miss was read: {}",
        report.note
    );
    let tail = format!(
        "{finished} finished and {closed} closed since the restart without coming back are not \
         expected back."
    );
    assert!(report.note.contains(&tail), "{}", report.note);
}

// ---------------------------------------------------------------------------
// Phase 4: owed or lost, decided by one awaiting-restore predicate
// ---------------------------------------------------------------------------

/// `restoreOwed` per outcome, on the top-level report and on this boot's
/// generation:
///
/// - an open row nothing brought back is still owed (the control: a fix
///   cannot pass by answering `false` for everything);
/// - a miss whose row closed `no-terminal` (it left the restore set) or
///   `pty-exit` (it stays in the set, for the next boot) since the boot is not;
/// - a `closed-since` row and a finished row are not;
/// - a back row is not asked: `null`, even though its row is in the restore
///   set and unfinished.
///
/// Mutations: `classify` serving `None` for every not-back outcome (the owed
/// control); `classify` reading `restore_owed` for back outcomes too (`back`
/// reads `true`).
#[test]
fn since_restart_restore_owed_says_which_sessions_this_boot_still_owes_a_restore() {
    let restart = Restart::new(&[
        "back",
        "owed",
        "no-terminal",
        "pty-exit",
        "closed-on-purpose",
        "finished",
    ]);
    restart.poll_saw_alive("back");
    restart
        .store
        .record_close("no-terminal", CloseReason::NoTerminal);
    restart.store.record_close("pty-exit", CloseReason::PtyExit);
    restart
        .store
        .record_close("closed-on-purpose", CloseReason::Explicit);
    restart
        .store
        .set_finished("finished", true, None)
        .expect("a known session");

    let report = restart.report(&["term-back", UNRELATED_LIVE_PANE], &[]);

    for (returned, missing, closed_since, finished) in [
        (
            &report.returned,
            &report.missing,
            &report.closed_since,
            &report.finished,
        ),
        (
            &report.generations[0].returned,
            &report.generations[0].missing,
            &report.generations[0].closed_since,
            &report.generations[0].finished,
        ),
    ] {
        assert_eq!(ids(returned), vec!["back"]);
        assert_eq!(ids(missing), vec!["no-terminal", "owed", "pty-exit"]);
        assert_eq!(ids(closed_since), vec!["closed-on-purpose"]);
        assert_eq!(ids(finished), vec!["finished"]);
        let owed = |bucket: &[LedgerOutcome], id: &str| {
            bucket
                .iter()
                .find(|o| o.claude_session_id == id)
                .unwrap_or_else(|| panic!("{id} in its bucket"))
                .restore_owed
        };
        assert_eq!(owed(missing, "owed"), Some(true), "the control");
        assert_eq!(owed(missing, "no-terminal"), Some(false));
        assert_eq!(
            owed(missing, "pty-exit"),
            Some(false),
            "closed by this process: no restore in it brings the row back"
        );
        assert_eq!(owed(closed_since, "closed-on-purpose"), Some(false));
        assert_eq!(owed(finished, "finished"), Some(false));
        assert_eq!(owed(returned, "back"), None, "a back session is not asked");
    }
}

/// Before the boot is classified, no miss's `restoreOwed` can be told: every
/// not-back outcome serves `null` (never `false`, which the strip would count
/// as "didn't come back"), and the note names the unread misses rather than
/// folding them into either side. Mutation: the note's unread clause dropped.
#[test]
fn since_restart_restore_owed_is_null_before_the_boot_is_classified() {
    let restart = Restart::new(&["owed", "no-terminal", "finished"]);
    restart
        .store
        .record_close("no-terminal", CloseReason::NoTerminal);
    restart
        .store
        .set_finished("finished", true, None)
        .expect("a known session");
    let unclassified = RestoreView {
        booted_at_ms: None,
        ..restart.restore_view()
    };

    let report = restart.report_under(
        &[UNRELATED_LIVE_PANE],
        ProcessRead::Held(Vec::new()),
        &unclassified,
    );

    assert_eq!(ids(&report.missing), vec!["no-terminal", "owed"]);
    for outcome in report.missing.iter().chain(&report.finished) {
        assert_eq!(outcome.restore_owed, None, "{}", outcome.claude_session_id);
    }
    assert!(
        report.note.contains(
            "2 did not — 0 still owed a restore by this boot, 0 will not be restored by it, \
             and for 2 that could not be read;"
        ),
        "{}",
        report.note
    );
}
