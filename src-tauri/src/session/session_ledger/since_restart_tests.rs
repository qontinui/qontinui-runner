//! Phase 0 of plan `2026-11-11-since-restart-roster-counts-closed-sessions-as-missing`:
//! the "Since restart" accounting defects, pinned as tests that are RED on the
//! code they were written against.
//!
//! Every test enters the way production does:
//!
//! - a real [`SessionLifecycleStore`] in a tempdir;
//! - the previous process's roster captured by the production [`capture`];
//! - the liveness poll's observation of a live session as the `touch` its
//!   `KeepAlive` writes (`main.rs`, the lifecycle poll's classify loop);
//! - closes through the production [`SessionLifecycleStore::record_close`] with
//!   the reasons production writes (`explicit`, `pty-exit`, `no-terminal`);
//! - the classification through [`observed_back`] + [`registry_now`] +
//!   [`diff`] / [`generation_reports`] — the sequence [`report`] runs. `report`
//!   itself is not callable here: it reads the process-global ledger
//!   generations and the latched boot census.
//!
//! Each assertion states the INTENDED behaviour (the plan's D1 and D2), never
//! today's output, and each test asserts a control first: a session nothing
//! brought back stays `missing`, so a fix cannot pass by reporting nothing
//! missing at all.
//!
//! Phases 2 and 3 turn them green. Phase 2's aftermath record is fed where these
//! tests observe a session alive ([`Restart::poll_saw_alive`], the poll's
//! `KeepAlive`) and from the live session registry (test 3's
//! `read_live_sessions` fixture); Phase 3's `classify` reads that record, which
//! is the input [`Restart::report`] gains.

use super::*;
use crate::session::claude_session_registry::read_live_sessions;
use crate::session::session_lifecycle_store::TerminalSessionRecord;

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
        // Everything a test does next happens strictly after the boot.
        first_ms_after(boot_at_ms);
        Self {
            dir,
            store,
            prior,
            boot_at_ms,
        }
    }

    /// The liveness poll found `id` alive this boot: its row matched a live pane
    /// and was `KeepAlive`d, which is the `touch` the poll writes. Phase 2's
    /// aftermath record is fed from the same observation.
    fn poll_saw_alive(&self, id: &str) {
        self.store.touch(id);
    }

    /// `GET /control/sessions/ledger` as [`report`] builds it, with `live_panes`
    /// the PTYs alive in this process now. The boot census is latched and the
    /// terminal manager answers, so a miss count here is a claim, not an upper
    /// bound (`restore_stamps_available` is true, as in `report`).
    fn report(&self, live_panes: &[&str]) -> LedgerReport {
        let live: HashSet<String> = live_panes.iter().map(|t| t.to_string()).collect();
        let back = observed_back(&self.store, Some(self.boot_at_ms), Some(&live));
        let now_state = registry_now(&self.store);
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
/// bucket, named here by the plan's D2 class (`closed-since`); today's report
/// field for it is `closed_by_user`, which Phase 3 renames `closed_since`.
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
            &report.closed_by_user,
            id,
        ),
        bucket_in(
            &generation.returned,
            &generation.missing,
            &generation.finished,
            &generation.closed_by_user,
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
    restart.store.record_close("came-back", "explicit");

    let report = restart.report(&[UNRELATED_LIVE_PANE]);

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
    let closes = [("exited", "pty-exit"), ("orphaned", "no-terminal")];
    let restart = Restart::new(&["exited", "orphaned", "lost"]);
    for (id, reason) in closes {
        restart.poll_saw_alive(id);
        restart.store.record_close(id, reason);
    }

    let report = restart.report(&[UNRELATED_LIVE_PANE]);

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    let not_back: Vec<(&str, &str, (&str, &str))> = closes
        .iter()
        .map(|(id, reason)| (*id, *reason, buckets(&report, id)))
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
/// Why the evidence is not injected into the classification: at `origin/main`
/// none of the three classification calls takes process evidence.
/// `observed_back` reads the store, this boot's instant and the live TERMINAL
/// set; `registry_now` reads the store; `diff` / `generation_reports` take the
/// resulting `back` list. The fixture below is the real reader's real input,
/// asserted to read as live. Phase 2 records it in the aftermath record from
/// the poll's `live_identity`, and Phase 3's `classify` reads that record;
/// handing `held` to [`Restart::report`] then turns this test green.
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

    let report = restart.report(&["term-new-pane", UNRELATED_LIVE_PANE]);

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

/// Defect 2 ("Why" 2): the closed-on-purpose class never fires. A session that
/// never came back and was then closed `explicit` (the webview's tab close) reads
/// `missing`, because `registry_now` files a session as closed on purpose only
/// for `user-close`, which no production code writes. Intended (D2): not
/// missing, in the deliberate-close bucket (`closed-since`).
#[test]
fn since_restart_a_session_closed_explicit_without_coming_back_is_closed_since_not_missing() {
    let restart = Restart::new(&["closed-on-purpose", "lost"]);
    restart.store.record_close("closed-on-purpose", "explicit");

    let report = restart.report(&[UNRELATED_LIVE_PANE]);
    let overlay = registry_now(&restart.store).closed_by_user;

    assert_eq!(
        buckets(&report, "lost"),
        ("missing", "missing"),
        "control: a session nothing brought back is missing"
    );
    assert_eq!(
        buckets(&report, "closed-on-purpose"),
        ("closed-since", "closed-since"),
        "a session closed `explicit` after the capture without coming back is closed on \
         purpose, never missing (registry_now's closed-on-purpose overlay holds: {overlay:?})"
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
    restart.store.record_close("came-back", "explicit");
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

    let report = restart.report(&[UNRELATED_LIVE_PANE]);

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
