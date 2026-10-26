//! Tests for [`super`] — the notice loop, the per-row state and the verdict.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::*;
use crate::coord_drain_state::DrainObservation;
use crate::mcp::session_message_poller::GateMiss;
use crate::restart_prep::cohort::{build_row, Cohort, CohortRow, PauseState, RowFacts};
use crate::restart_prep::prepare::{
    HoldCall, HoldDoor, HoldRequest, HoldRole, ReleaseCall, RestartPrep,
};
use crate::restart_prep::{persist_to, HoldStatus, PrepState, PrepareRecord, RECORD_FILE};
use crate::session::claude_activity::Activity;
use crate::session::session_ledger::LedgerEntry;
use crate::terminal::graceful_exit::probe_fixtures::{
    screen as probe_screen, BUSY_QUEUED, BUSY_STREAMING, BUSY_TOOL_CALL,
};
use crate::terminal::graceful_exit::ScreenText;

const T0: i64 = 1_790_000_000_000;
const PREP: &str = "prep-4";

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// What a fake pane shows; mutable between ticks.
#[derive(Clone)]
struct PaneCfg {
    quiescent: bool,
    live: bool,
    screen: ScreenText,
}

struct FakePane {
    cfg: PaneCfg,
    submitted: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl InputPane for FakePane {
    async fn looks_idle_quiescent(&self) -> bool {
        self.cfg.quiescent
    }
    async fn claude_live(&self) -> Result<(), GateMiss> {
        if self.cfg.live {
            Ok(())
        } else {
            Err(GateMiss::ClaudeNotLive)
        }
    }
    fn screen(&self) -> ScreenText {
        self.cfg.screen.clone()
    }
}

#[async_trait]
impl NoticePane for FakePane {
    fn input(&self) -> &dyn InputPane {
        self
    }
    async fn submit(&self, text: &str) -> Result<(), String> {
        self.submitted.lock().unwrap().push(text.to_string());
        Ok(())
    }
}

/// Terminal id → pane config; every submit lands in one shared log keyed by
/// terminal id.
#[derive(Default)]
struct FakeTerminals {
    panes: Mutex<HashMap<String, PaneCfg>>,
    submitted: Mutex<HashMap<String, Arc<Mutex<Vec<String>>>>>,
}

impl FakeTerminals {
    fn with(panes: Vec<(&str, PaneCfg)>) -> Self {
        let t = Self::default();
        for (id, cfg) in panes {
            t.set(id, cfg);
        }
        t
    }
    fn set(&self, terminal_id: &str, cfg: PaneCfg) {
        self.panes
            .lock()
            .unwrap()
            .insert(terminal_id.to_string(), cfg);
    }
    fn sent(&self, terminal_id: &str) -> Vec<String> {
        self.submitted
            .lock()
            .unwrap()
            .get(terminal_id)
            .map(|l| l.lock().unwrap().clone())
            .unwrap_or_default()
    }
}

impl NoticeTerminals for FakeTerminals {
    fn pane(&self, terminal_id: &str) -> Option<Box<dyn NoticePane>> {
        let cfg = self.panes.lock().unwrap().get(terminal_id)?.clone();
        let log = self
            .submitted
            .lock()
            .unwrap()
            .entry(terminal_id.to_string())
            .or_default()
            .clone();
        Some(Box::new(FakePane {
            cfg,
            submitted: log,
        }))
    }
}

fn screen(rows: &[&str], cursor_row: u16, cursor_col: u16) -> ScreenText {
    ScreenText {
        lines: rows.iter().map(|s| s.to_string()).collect(),
        cursor_row,
        cursor_col,
    }
}

/// A finished turn at an empty prompt.
fn idle_screen() -> ScreenText {
    screen(&["● done.", "────", "❯\u{a0}", "────"], 2, 2)
}

fn idle_pane() -> PaneCfg {
    PaneCfg {
        quiescent: true,
        live: true,
        screen: idle_screen(),
    }
}

/// The real Claude Code 2.1.306 busy screen (marker on the status line).
fn busy_pane(raw: &str) -> PaneCfg {
    PaneCfg {
        quiescent: false,
        live: true,
        screen: probe_screen(raw),
    }
}

fn entry(id: &str) -> LedgerEntry {
    LedgerEntry {
        claude_session_id: id.to_string(),
        terminal_id: format!("term-{id}"),
        page_id: "page-a".to_string(),
        zone_index: 0,
        title: Some(format!("tab-{id}")),
        session_name: Some(format!("name-{id}")),
        account_label: Some("iris".to_string()),
        config_dir: None,
        working_dir: None,
        restorable: None,
        worktree_path: None,
        plan_slug: None,
        work_unit_id: None,
        wip_state: None,
        wip_ref: None,
        custody_session_mismatch: false,
        name_source: None,
        resume_config_dir: None,
        resume_command: None,
        provider: None,
        last_seen_at: None,
        finished: false,
    }
}

fn row(id: &str) -> CohortRow {
    build_row(
        entry(id),
        RowFacts {
            finished: Some(false),
            provider: Some("claude".to_string()),
            ..Default::default()
        },
    )
}

fn cohort(rows: Vec<CohortRow>, at: i64) -> Cohort {
    Cohort {
        captured_at_ms: at,
        captured_at: rfc3339(at),
        reason: crate::restart_prep::CAPTURE_REASON.to_string(),
        rows,
    }
}

fn record(rows: Vec<CohortRow>, hold: HoldStatus) -> PrepareRecord {
    let mut r = PrepareRecord::new(PREP.to_string(), T0, cohort(rows, T0));
    r.hold = hold;
    r.hold_until_ms = Some(T0 + crate::restart_prep::prepare::DEFAULT_HOLD_MS);
    r.hold_until = Some(rfc3339(T0 + crate::restart_prep::prepare::DEFAULT_HOLD_MS));
    r
}

/// A controller over `rec`, counting `restart-prep-changed` announcements.
fn controller(dir: &tempfile::TempDir, rec: &PrepareRecord) -> (RestartPrep, Arc<AtomicUsize>) {
    let path = dir.path().join(RECORD_FILE);
    persist_to(&path, rec).unwrap();
    let emits = Arc::new(AtomicUsize::new(0));
    let counter = emits.clone();
    let ctl = RestartPrep::load(
        path,
        Some(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })),
    );
    (ctl, emits)
}

/// The re-capture a tick folds in: the same sessions, captured at `at`.
fn recapture(ids: &[&str], at: i64) -> Result<Cohort, String> {
    Ok(cohort(ids.iter().map(|id| row(id)).collect(), at))
}

fn proc(activity: Activity, children: Option<bool>) -> ProcEvidence {
    ProcEvidence {
        pid: 0,
        has_live_children: children,
        coord_finished: false,
        activity,
    }
}

fn census(procs: Vec<(&str, ProcEvidence)>) -> CensusEvidence {
    CensusEvidence {
        census_error: None,
        live_terminals: procs.iter().map(|(id, _)| format!("term-{id}")).collect(),
        // Each session its own pid, unless the test chose one.
        by_session: procs
            .into_iter()
            .enumerate()
            .map(|(i, (id, mut p))| {
                if p.pid == 0 {
                    p.pid = 1000 + i as u32;
                }
                (id.to_string(), p)
            })
            .collect(),
        unattributed_live: 0,
        device_drain: Some("drained".to_string()),
        close_reasons: HashMap::new(),
    }
}

fn row_of<'a>(rec: &'a PrepareRecord, id: &str) -> &'a CohortRow {
    rec.cohort
        .rows
        .iter()
        .find(|r| r.session.claude_session_id == id)
        .expect("row")
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restart_prep_an_idle_session_is_notified_once() {
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![row("a")], HoldStatus::Confirmed));
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);

    let first = ctl
        .notice_tick(&terms, recapture(&["a"], T0 + 1_000), &ev, T0 + 1_000)
        .await;
    assert!(
        matches!(first, TickOutcome::Ran { delivered: 1, .. }),
        "{first:?}"
    );
    let sent = terms.sent("term-a");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0], notice_text(PREP));
    assert!(sent[0].contains(PREP), "the notice names the prepare");
    let rec = ctl.record().unwrap();
    let a = row_of(&rec, "a");
    assert_eq!(a.notified_at, Some(T0 + 1_000));
    assert_eq!(a.notice_arm.as_deref(), Some("idle"));
    assert_eq!(a.pause_state, Some(PauseState::Notified));

    // The pane still admits a paste on every later tick: the guard is the
    // row's own `notified_at`, never the screen.
    for i in 2..5 {
        let at = T0 + i * 15_000;
        let again = ctl
            .notice_tick(&terms, recapture(&["a"], at), &ev, at)
            .await;
        assert!(
            matches!(again, TickOutcome::Ran { delivered: 0, .. }),
            "{again:?}"
        );
    }
    assert_eq!(
        terms.sent("term-a").len(),
        1,
        "a second tick never re-sends"
    );
    let rec = ctl.record().unwrap();
    assert_eq!(row_of(&rec, "a").notified_at, Some(T0 + 1_000));
    assert_eq!(row_of(&rec, "a").pause_state, Some(PauseState::Paused));
}

#[tokio::test]
async fn restart_prep_a_working_session_is_told_through_the_busy_arm() {
    for raw in [BUSY_TOOL_CALL, BUSY_STREAMING] {
        let dir = tempfile::tempdir().unwrap();
        let (ctl, _) = controller(&dir, &record(vec![row("w")], HoldStatus::Confirmed));
        let terms = FakeTerminals::with(vec![("term-w", busy_pane(raw))]);
        let ev = census(vec![("w", proc(Activity::Working, Some(true)))]);
        let out = ctl
            .notice_tick(&terms, recapture(&["w"], T0 + 1), &ev, T0 + 1)
            .await;
        assert!(
            matches!(
                out,
                TickOutcome::Ran {
                    delivered: 1,
                    ready_to_restart: false,
                    ..
                }
            ),
            "{out:?}"
        );
        let rec = ctl.record().unwrap();
        assert_eq!(row_of(&rec, "w").notice_arm.as_deref(), Some("busy_prompt"));
        assert_eq!(terms.sent("term-w").len(), 1);

        // Claude Code queued it: the input row now reads `Press up to edit
        // queued messages`. Nothing more is typed.
        terms.set("term-w", busy_pane(BUSY_QUEUED));
        ctl.notice_tick(&terms, recapture(&["w"], T0 + 2), &ev, T0 + 2)
            .await;
        assert_eq!(terms.sent("term-w").len(), 1);
        let rec = ctl.record().unwrap();
        assert_eq!(row_of(&rec, "w").pause_state, Some(PauseState::Notified));
    }
}

#[tokio::test]
async fn restart_prep_a_dialog_or_a_draft_gets_nothing_and_is_retried() {
    let mut dialog = probe_screen(BUSY_TOOL_CALL);
    dialog.lines[68] = " ❯ 1. Yes".to_string();
    dialog.cursor_col = 3;
    let mut draft = idle_screen();
    draft.lines[2] = "❯\u{a0}half a thought".to_string();
    draft.cursor_col = 16;
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(
        &dir,
        &record(vec![row("d"), row("h")], HoldStatus::Confirmed),
    );
    let terms = FakeTerminals::with(vec![
        (
            "term-d",
            PaneCfg {
                quiescent: false,
                live: true,
                screen: dialog,
            },
        ),
        (
            "term-h",
            PaneCfg {
                quiescent: true,
                live: true,
                screen: draft,
            },
        ),
    ]);
    let ev = census(vec![
        ("d", proc(Activity::Working, Some(true))),
        ("h", proc(Activity::Idle, Some(false))),
    ]);
    let out = ctl
        .notice_tick(&terms, recapture(&["d", "h"], T0 + 1), &ev, T0 + 1)
        .await;
    assert!(
        matches!(out, TickOutcome::Ran { delivered: 0, .. }),
        "{out:?}"
    );
    assert!(
        terms.sent("term-d").is_empty(),
        "a selected choice gets nothing"
    );
    assert!(
        terms.sent("term-h").is_empty(),
        "a half-typed prompt gets nothing"
    );
    let rec = ctl.record().unwrap();
    for id in ["d", "h"] {
        let r = row_of(&rec, id);
        assert_eq!(r.notified_at, None, "{id}");
        assert!(r.notice_deferred.is_some(), "{id}: the refusal is named");
        assert_eq!(r.pause_state, Some(PauseState::Working), "{id}");
        assert!(
            r.pause_reason
                .as_deref()
                .unwrap()
                .starts_with("not told yet"),
            "{id}: {:?}",
            r.pause_reason
        );
    }

    // The dialog closes and the operator sends the draft: both panes admit a
    // paste now, and the next tick delivers.
    terms.set("term-d", busy_pane(BUSY_TOOL_CALL));
    terms.set("term-h", idle_pane());
    let out = ctl
        .notice_tick(&terms, recapture(&["d", "h"], T0 + 2), &ev, T0 + 2)
        .await;
    assert!(
        matches!(out, TickOutcome::Ran { delivered: 2, .. }),
        "{out:?}"
    );
    assert_eq!(terms.sent("term-d").len(), 1);
    assert_eq!(terms.sent("term-h").len(), 1);
    let rec = ctl.record().unwrap();
    assert_eq!(row_of(&rec, "d").notice_deferred, None);
}

#[tokio::test]
async fn restart_prep_rows_that_are_not_claude_or_have_no_terminal_are_labelled_and_skipped() {
    let mut codex = row("x");
    codex.provider = Some("codex".to_string());
    let mut gpt = row("g");
    gpt.model = Some("gpt-5".to_string());
    let gone = row("n");
    let finished = build_row(
        entry("f"),
        RowFacts {
            finished: Some(true),
            ..Default::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(
        &dir,
        &record(vec![codex, gpt, gone, finished], HoldStatus::Confirmed),
    );
    let terms = FakeTerminals::with(vec![
        ("term-x", idle_pane()),
        ("term-g", idle_pane()),
        ("term-f", idle_pane()),
    ]);
    // `n`'s process is in the census, but no live terminal holds its pane.
    let ev = census(vec![
        ("x", proc(Activity::Idle, Some(false))),
        ("g", proc(Activity::Idle, Some(false))),
        ("n", proc(Activity::Idle, Some(false))),
        ("f", proc(Activity::Idle, Some(false))),
    ]);
    let pinned = ctl.record().unwrap().cohort;
    ctl.notice_tick(
        &terms,
        Ok(Cohort {
            captured_at_ms: T0 + 1,
            ..pinned
        }),
        &ev,
        T0 + 1,
    )
    .await;
    for t in ["term-x", "term-g", "term-n", "term-f"] {
        assert!(terms.sent(t).is_empty(), "{t}");
    }
    let rec = ctl.record().unwrap();
    assert!(row_of(&rec, "x")
        .notice_skipped
        .as_deref()
        .unwrap()
        .contains("provider `codex`"));
    assert!(row_of(&rec, "g")
        .notice_skipped
        .as_deref()
        .unwrap()
        .contains("model `gpt-5`"));
    assert!(row_of(&rec, "n")
        .notice_skipped
        .as_deref()
        .unwrap()
        .contains("no live terminal"));
    assert_eq!(
        row_of(&rec, "f").notice_skipped,
        None,
        "a finished row is not owed a notice at all"
    );
    assert_eq!(row_of(&rec, "f").pause_state, Some(PauseState::Finished));
}

// ---------------------------------------------------------------------------
// Row state and the verdict
// ---------------------------------------------------------------------------

/// Every row state the census can produce, and whether it blocks.
#[test]
fn restart_prep_verdict_truth_table_over_every_row_state() {
    struct Case {
        name: &'static str,
        notified: bool,
        limit_hit: bool,
        finished_local: bool,
        left_capture: bool,
        proc: Option<ProcEvidence>,
        coord_finished: bool,
        live_terminal: bool,
        unattributed: usize,
        census_error: bool,
        want: PauseState,
        blocks: bool,
    }
    let base = Case {
        name: "",
        notified: true,
        limit_hit: false,
        finished_local: false,
        left_capture: false,
        proc: None,
        coord_finished: false,
        live_terminal: true,
        unattributed: 0,
        census_error: false,
        want: PauseState::Unknown,
        blocks: true,
    };
    let cases = vec![
        Case {
            name: "told, idle, no children",
            proc: Some(proc(Activity::Idle, Some(false))),
            want: PauseState::Paused,
            blocks: false,
            ..base
        },
        Case {
            name: "told, idle, a child attached (children are reported, not required)",
            proc: Some(proc(Activity::Idle, Some(true))),
            want: PauseState::Paused,
            blocks: false,
            ..base
        },
        Case {
            name: "told, still working",
            proc: Some(proc(Activity::Working, Some(false))),
            want: PauseState::Notified,
            ..base
        },
        Case {
            name: "told, stale",
            proc: Some(proc(Activity::Stale, Some(false))),
            want: PauseState::Notified,
            ..base
        },
        Case {
            name: "told, idle, children NULL",
            proc: Some(proc(Activity::Idle, None)),
            want: PauseState::Paused,
            blocks: false,
            ..base
        },
        Case {
            name: "told, activity unknown",
            proc: Some(proc(Activity::Unknown, Some(false))),
            want: PauseState::Unknown,
            ..base
        },
        Case {
            name: "not told, idle",
            notified: false,
            proc: Some(proc(Activity::Idle, Some(false))),
            want: PauseState::Working,
            ..base
        },
        Case {
            name: "not told, working",
            notified: false,
            proc: Some(proc(Activity::Working, Some(true))),
            want: PauseState::Working,
            ..base
        },
        Case {
            name: "not told, children NULL",
            notified: false,
            proc: Some(proc(Activity::Working, None)),
            want: PauseState::Working,
            ..base
        },
        Case {
            name: "finished (local mark)",
            finished_local: true,
            proc: Some(proc(Activity::Working, Some(true))),
            want: PauseState::Finished,
            blocks: false,
            ..base
        },
        Case {
            name: "finished (coord axis)",
            proc: Some(proc(Activity::Working, None)),
            coord_finished: true,
            want: PauseState::Finished,
            blocks: false,
            ..base
        },
        Case {
            name: "finished, wound down",
            finished_local: true,
            left_capture: true,
            want: PauseState::Closed,
            blocks: false,
            ..base
        },
        Case {
            name: "closed since the prepare",
            left_capture: true,
            want: PauseState::Closed,
            blocks: false,
            ..base
        },
        Case {
            name: "no process, no terminal",
            live_terminal: false,
            want: PauseState::Closed,
            blocks: false,
            ..base
        },
        Case {
            name: "no process, terminal live",
            want: PauseState::Unknown,
            ..base
        },
        Case {
            name: "no process, an unattributed process exists",
            live_terminal: false,
            unattributed: 1,
            want: PauseState::Unknown,
            ..base
        },
        Case {
            name: "limit hit, idle",
            limit_hit: true,
            proc: Some(proc(Activity::Idle, Some(false))),
            want: PauseState::LimitHit,
            blocks: false,
            ..base
        },
        Case {
            name: "limit hit, working",
            limit_hit: true,
            proc: Some(proc(Activity::Working, Some(false))),
            want: PauseState::LimitHit,
            ..base
        },
        Case {
            name: "limit hit, idle, children NULL",
            limit_hit: true,
            proc: Some(proc(Activity::Idle, None)),
            want: PauseState::LimitHit,
            blocks: false,
            ..base
        },
        Case {
            name: "limit hit, activity unknown",
            limit_hit: true,
            proc: Some(proc(Activity::Unknown, Some(false))),
            want: PauseState::LimitHit,
            ..base
        },
        Case {
            name: "census unavailable",
            proc: Some(proc(Activity::Idle, Some(false))),
            census_error: true,
            want: PauseState::Unknown,
            ..base
        },
    ];
    for c in cases {
        let mut r = row("s");
        if c.notified {
            r.notified_at = Some(T0);
        }
        if c.limit_hit {
            r.state_at_prepare = crate::restart_prep::cohort::StateAtPrepare::LimitHit;
        }
        if c.finished_local {
            r.finished = Some(true);
        }
        if c.left_capture {
            r.left_capture_at = Some(T0);
        }
        let mut ev = census(vec![]);
        if let Some(mut p) = c.proc {
            p.coord_finished = c.coord_finished;
            ev.by_session.insert("s".to_string(), p);
        }
        if c.live_terminal {
            ev.live_terminals.insert("term-s".to_string());
        }
        ev.unattributed_live = c.unattributed;
        if c.census_error {
            ev.census_error = Some("process table unreadable".to_string());
        }
        let (state, _, reason) = row_state(&r, &ev);
        assert_eq!(state, c.want, "{}: {reason}", c.name);

        let mut rec = record(vec![r], HoldStatus::Confirmed);
        refresh_rows(&mut rec.cohort, &ev);
        let v = verdict_of(&rec, &ev, None, T0 + 1);
        let row_blocks = v.blockers.iter().any(|b| b.session.as_deref() == Some("s"));
        assert_eq!(row_blocks, c.blocks, "{}: {:?}", c.name, v.blockers);
        if !c.census_error && c.unattributed == 0 {
            assert_eq!(
                v.ready_to_restart, !c.blocks,
                "{}: {:?}",
                c.name, v.blockers
            );
        } else {
            assert!(
                !v.ready_to_restart,
                "{}: an unavailable census or an unattributed process is never ready",
                c.name
            );
        }
        if c.blocks {
            let b = v
                .blockers
                .iter()
                .find(|b| b.session.as_deref() == Some("s"))
                .unwrap();
            assert_eq!(b.display_name.as_deref(), Some("name-s"), "{}", c.name);
        }
    }
}

#[test]
fn restart_prep_an_unconfirmed_hold_is_not_ready() {
    let mut paused = row("p");
    paused.notified_at = Some(T0);
    let ev = census(vec![("p", proc(Activity::Idle, Some(false)))]);
    let verdict_for = |hold: HoldStatus| {
        let mut rec = record(vec![paused.clone()], hold);
        refresh_rows(&mut rec.cohort, &ev);
        verdict_of(&rec, &ev, None, T0 + 1)
    };
    for ok in [
        HoldStatus::Confirmed,
        HoldStatus::Local,
        HoldStatus::LocalSecondary,
    ] {
        let v = verdict_for(ok.clone());
        assert!(v.ready_to_restart, "{ok:?}: {:?}", v.blockers);
    }
    for not in [
        HoldStatus::Unconfirmed {
            cause: "coord_unreachable: timeout".to_string(),
        },
        HoldStatus::Unconfirmed {
            cause: "lane_held_by_other".to_string(),
        },
        HoldStatus::NotAttempted,
        HoldStatus::ReleaseSent {
            outcome: "released".to_string(),
        },
    ] {
        let v = verdict_for(not.clone());
        assert!(!v.ready_to_restart, "{not:?}");
        assert_eq!(v.blockers.len(), 1, "{:?}", v.blockers);
        assert_eq!(v.blockers[0].session, None, "a prepare-wide blocker");
        assert!(
            v.blockers[0].reason.contains("not confirmed"),
            "{}",
            v.blockers[0].reason
        );
    }
    // Confirmed once, but this runner no longer reads the device drained.
    for reading in [Some("clear"), Some("unknown"), None] {
        let mut rec = record(vec![paused.clone()], HoldStatus::Confirmed);
        let mut gone = ev.clone();
        gone.device_drain = reading.map(str::to_string);
        refresh_rows(&mut rec.cohort, &gone);
        let v = verdict_of(&rec, &gone, None, T0 + 1);
        assert!(!v.ready_to_restart, "{reading:?}");
        assert!(
            v.blockers[0].reason.contains("no longer observed"),
            "{:?}",
            v.blockers
        );
        // A runner-local hold does not depend on coord's drain.
        let mut local = rec.clone();
        local.hold = HoldStatus::Local;
        assert!(verdict_of(&local, &gone, None, T0 + 1).ready_to_restart);
    }
    // A confirmed hold past its `until` is not a hold.
    let mut rec = record(vec![paused.clone()], HoldStatus::Confirmed);
    refresh_rows(&mut rec.cohort, &ev);
    let past = rec.hold_until_ms.unwrap() + 1;
    let v = verdict_of(&rec, &ev, None, past);
    assert!(!v.ready_to_restart);
    assert!(v.hold.starts_with("hold expired at"), "{}", v.hold);
}

#[test]
fn restart_prep_the_verdict_waits_for_a_recapture_and_names_uncaptured_sessions() {
    let mut paused = row("p");
    paused.notified_at = Some(T0);
    let ev = census(vec![("p", proc(Activity::Idle, Some(false)))]);
    let mut rec = record(vec![paused], HoldStatus::Confirmed);
    refresh_rows(&mut rec.cohort, &ev);
    assert!(verdict_of(&rec, &ev, None, T0 + 1).ready_to_restart);

    // A row change after the last re-capture (a notice, a limit_hit mark).
    rec.last_change_at_ms = Some(rec.cohort.captured_at_ms + 1);
    let v = verdict_of(&rec, &ev, None, T0 + 2);
    assert!(!v.ready_to_restart);
    assert!(
        v.blockers[0].reason.contains("re-captured"),
        "{:?}",
        v.blockers
    );
    rec.last_change_at_ms = Some(rec.cohort.captured_at_ms);
    assert!(verdict_of(&rec, &ev, None, T0 + 2).ready_to_restart);

    // A failed re-capture.
    let v = verdict_of(&rec, &ev, Some("lifecycle store not available"), T0 + 2);
    assert!(!v.ready_to_restart);

    // A live session the cohort does not hold.
    let mut wider = ev.clone();
    wider
        .by_session
        .insert("late".to_string(), proc(Activity::Working, Some(true)));
    let v = verdict_of(&rec, &wider, None, T0 + 2);
    assert!(!v.ready_to_restart);
    assert_eq!(v.blockers[0].session.as_deref(), Some("late"));

    // A live process the census could not attribute to any session.
    let mut stray = ev.clone();
    stray.unattributed_live = 2;
    let v = verdict_of(&rec, &stray, None, T0 + 2);
    assert!(!v.ready_to_restart);
    assert!(
        v.blockers[0].reason.starts_with("2 live"),
        "{:?}",
        v.blockers
    );

    // Zero rows, a confirmed hold, a readable census: nothing to pause.
    let empty = record(vec![], HoldStatus::Confirmed);
    assert!(verdict_of(&empty, &census(vec![]), None, T0 + 1).ready_to_restart);
    // ... but not when the census could not be taken.
    assert!(!verdict_of(&empty, &CensusEvidence::unavailable("x"), None, T0 + 1).ready_to_restart);
    // Not in its window.
    let mut cancelled = empty.clone();
    cancelled.state = PrepState::Cancelled;
    assert!(!verdict_of(&cancelled, &census(vec![]), None, T0 + 1).ready_to_restart);
}

#[tokio::test]
async fn restart_prep_a_tick_moves_preparing_to_ready_and_back_and_announces_only_changes() {
    let mut a = row("a");
    a.notified_at = Some(T0);
    let dir = tempfile::tempdir().unwrap();
    let (ctl, emits) = controller(&dir, &record(vec![a], HoldStatus::Confirmed));
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let idle = census(vec![("a", proc(Activity::Idle, Some(false)))]);

    let out = ctl
        .notice_tick(&terms, recapture(&["a"], T0 + 10), &idle, T0 + 10)
        .await;
    assert!(
        matches!(
            out,
            TickOutcome::Ran {
                delivered: 0,
                ready_to_restart: true,
                changed: true
            }
        ),
        "{out:?}"
    );
    let rec = ctl.record().unwrap();
    assert_eq!(rec.state, PrepState::Ready);
    assert_eq!(rec.verdict.as_ref().unwrap().state, PrepState::Ready);
    assert_eq!(emits.load(Ordering::SeqCst), 1);

    // The same world again: nothing visible changed, nothing announced.
    let out = ctl
        .notice_tick(&terms, recapture(&["a"], T0 + 20), &idle, T0 + 20)
        .await;
    assert!(
        matches!(out, TickOutcome::Ran { changed: false, .. }),
        "{out:?}"
    );
    assert_eq!(emits.load(Ordering::SeqCst), 1);

    // The session picks work back up: back to preparing, announced.
    let working = census(vec![("a", proc(Activity::Working, Some(true)))]);
    ctl.notice_tick(&terms, recapture(&["a"], T0 + 30), &working, T0 + 30)
        .await;
    let rec = ctl.record().unwrap();
    assert_eq!(rec.state, PrepState::Preparing);
    assert_eq!(row_of(&rec, "a").pause_state, Some(PauseState::Notified));
    assert_eq!(emits.load(Ordering::SeqCst), 2);
    assert_eq!(
        terms.sent("term-a").len(),
        0,
        "an already-told row is never re-told"
    );

    // A limit hit marked between ticks: the fresh read waits for a re-capture.
    ctl.freeze_hit("a", None, T0 + 40);
    let rec = ctl.record().unwrap();
    let fresh = evaluate_fresh(&rec, &idle, T0 + 41);
    assert!(
        !fresh.verdict.ready_to_restart,
        "{:?}",
        fresh.verdict.blockers
    );
    // The next tick re-captures, keeps the mark, and reads limit_hit-and-idle.
    ctl.notice_tick(&terms, recapture(&["a"], T0 + 50), &idle, T0 + 50)
        .await;
    let rec = ctl.record().unwrap();
    assert_eq!(row_of(&rec, "a").pause_state, Some(PauseState::LimitHit));
    assert!(rec.verdict.unwrap().ready_to_restart);
}

#[tokio::test]
async fn restart_prep_a_new_session_joins_the_cohort_and_is_told() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = row("a");
    a.notified_at = Some(T0);
    let (ctl, _) = controller(&dir, &record(vec![a], HoldStatus::Local));
    let terms = FakeTerminals::with(vec![("term-a", idle_pane()), ("term-b", idle_pane())]);
    let ev = census(vec![
        ("a", proc(Activity::Idle, Some(false))),
        ("b", proc(Activity::Idle, Some(false))),
    ]);
    ctl.notice_tick(&terms, recapture(&["a", "b"], T0 + 1), &ev, T0 + 1)
        .await;
    let rec = ctl.record().unwrap();
    assert_eq!(rec.cohort.rows.len(), 2);
    assert_eq!(row_of(&rec, "b").notified_at, Some(T0 + 1));
    assert_eq!(terms.sent("term-b").len(), 1);
    assert_eq!(terms.sent("term-a").len(), 0);
    assert!(
        !rec.verdict.unwrap().ready_to_restart,
        "b was told this tick"
    );
}

#[tokio::test]
async fn restart_prep_an_expired_hold_sends_no_notices() {
    let dir = tempfile::tempdir().unwrap();
    let rec = record(vec![row("a")], HoldStatus::Confirmed);
    let past = rec.hold_until_ms.unwrap() + 1;
    let (ctl, _) = controller(&dir, &rec);
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    ctl.notice_tick(&terms, recapture(&["a"], past), &ev, past)
        .await;
    assert!(terms.sent("term-a").is_empty());
}

// ---------------------------------------------------------------------------
// Cancel
// ---------------------------------------------------------------------------

/// A door the cancel of a runner-local hold never calls.
struct NoCoord;

#[async_trait]
impl HoldDoor for NoCoord {
    fn role(&self) -> HoldRole {
        HoldRole::NotEnrolled {
            why: "test".to_string(),
        }
    }
    async fn hold(&self, _: &HoldRequest) -> HoldCall {
        panic!("no hold in this test")
    }
    async fn release(&self, _: &str) -> ReleaseCall {
        panic!("a local hold is released without coord")
    }
    async fn read_drain(&self) -> Result<DrainObservation, String> {
        panic!("no read in this test")
    }
}

#[tokio::test]
async fn restart_prep_cancel_sends_one_resume_line_only_to_told_rows() {
    let mut told = row("t");
    told.notified_at = Some(T0);
    let mut also = row("u");
    also.notified_at = Some(T0);
    let untold = row("n");
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![told, also, untold], HoldStatus::Local));
    // `u`'s operator is typing: the first pass cannot reach it.
    let mut draft = idle_screen();
    draft.lines[2] = "❯\u{a0}wait".to_string();
    draft.cursor_col = 6;
    let terms = FakeTerminals::with(vec![
        ("term-t", idle_pane()),
        (
            "term-u",
            PaneCfg {
                quiescent: true,
                live: true,
                screen: draft,
            },
        ),
        ("term-n", idle_pane()),
    ]);
    let cancelled = ctl
        .cancel(&NoCoord, T0 + 100)
        .await
        .expect("a local hold settles");
    assert_eq!(cancelled.state, PrepState::Cancelled);
    assert!(RestartPrep::owes_resume(&cancelled, T0 + 100));

    let out = ctl.resume_tick(&terms, T0 + 101).await;
    assert_eq!(out, TickOutcome::Resumed { delivered: 1 });
    assert_eq!(terms.sent("term-t"), vec![resume_text(PREP)]);
    assert!(terms.sent("term-u").is_empty());
    assert!(
        terms.sent("term-n").is_empty(),
        "a row never told gets nothing"
    );

    // `u` frees up; the next pass reaches it, and `t` is not sent twice.
    terms.set("term-u", idle_pane());
    let out = ctl.resume_tick(&terms, T0 + 115).await;
    assert_eq!(out, TickOutcome::Resumed { delivered: 1 });
    assert_eq!(terms.sent("term-t").len(), 1, "once");
    assert_eq!(terms.sent("term-u").len(), 1);
    let rec = ctl.record().unwrap();
    assert!(!RestartPrep::owes_resume(&rec, T0 + 120));
    assert_eq!(
        ctl.resume_tick(&terms, T0 + 130).await,
        TickOutcome::Idle(PrepState::Cancelled)
    );
    assert_eq!(terms.sent("term-t").len(), 1);
    assert_eq!(terms.sent("term-n").len(), 0);
    // The cancelled record takes no more save-and-pause notices either.
    let ev = census(vec![("n", proc(Activity::Idle, Some(false)))]);
    assert_eq!(
        ctl.notice_tick(&terms, recapture(&["n"], T0 + 140), &ev, T0 + 140)
            .await,
        TickOutcome::Idle(PrepState::Cancelled)
    );
    assert!(terms.sent("term-n").is_empty());
}

#[tokio::test]
async fn restart_prep_the_resume_line_stops_after_its_window() {
    let mut told = row("t");
    told.notified_at = Some(T0);
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![told], HoldStatus::Local));
    ctl.cancel(&NoCoord, T0).await.unwrap();
    let terms = FakeTerminals::with(vec![("term-t", idle_pane())]);
    let late = T0 + RESUME_NOTICE_WINDOW_MS + 1;
    assert_eq!(
        ctl.resume_tick(&terms, late).await,
        TickOutcome::Idle(PrepState::Cancelled)
    );
    assert!(terms.sent("term-t").is_empty());
}

// ---------------------------------------------------------------------------
// Census join and the readiness block
// ---------------------------------------------------------------------------

#[test]
fn restart_prep_the_census_join_reads_each_rows_own_process() {
    use crate::mcp::restart_readiness::ActivityEvidence;
    use crate::session::tracking_health::{LiveClaudeProcess, TrackingHealthReport};
    let process = |pid: u32, sid: Option<&str>, children: Option<bool>, status: Option<&str>| {
        LiveClaudeProcess {
            pid,
            parent_pid: None,
            image: Some("claude".to_string()),
            age_s: Some(60),
            cwd: None,
            has_live_children: children,
            nested_under_claude: false,
            session_id: sid.map(str::to_string),
            session_status: status.map(str::to_string),
            blocks_restart: true,
            wind_down: None,
        }
    };
    let mut nested = process(14, Some("a"), Some(true), None);
    nested.nested_under_claude = true;
    let report = TrackingHealthReport {
        terminal_hosted: vec![
            process(11, Some("a"), Some(false), Some("working")),
            nested,
            process(12, Some("f"), None, Some("finished")),
            process(13, None, Some(false), None),
        ],
        checked_at_ms: T0,
        live_claude_total: 4,
        tracked_open_total: 2,
        ai_plane: vec![],
        headless_exempt: vec![],
        live_untracked: vec![],
        tracked_dead: vec![],
        holder_hosted: vec![],
        holder_panes: vec![],
        holder_census_error: None,
    };
    let ev = evidence_from(
        Some(&report),
        None,
        &ActivityEvidence::default(),
        HashSet::new(),
        Some("drained".to_string()),
    );
    assert_eq!(ev.census_error, None);
    assert_eq!(
        ev.by_session.len(),
        2,
        "the nested subagent is not its own row"
    );
    let a = ev.by_session["a"];
    assert_eq!(a.has_live_children, Some(false), "the top-level process's");
    assert!(!a.coord_finished);
    assert_eq!(
        a.activity,
        Activity::Unknown,
        "no pane and no record: unknown, never idle"
    );
    assert!(ev.by_session["f"].coord_finished);
    assert_eq!(ev.unattributed_live, 1, "the unattributed process");

    let none = evidence_from(
        None,
        Some("boot".to_string()),
        &ActivityEvidence::default(),
        HashSet::new(),
        None,
    );
    assert_eq!(none.census_error.as_deref(), Some("boot"));
    assert_eq!(none.device_drain, None);
}

#[test]
fn restart_prep_the_readiness_block_is_flat_and_names_its_source() {
    let mut paused = row("p");
    paused.notified_at = Some(T0);
    let rec = record(vec![paused, row("w")], HoldStatus::Confirmed);
    let ev = census(vec![
        ("p", proc(Activity::Idle, Some(false))),
        ("w", proc(Activity::Working, Some(true))),
    ]);
    let block = evaluate_fresh(&rec, &ev, T0 + 5);
    let json = serde_json::to_value(&block).unwrap();
    assert_eq!(json["ready_to_restart"], false);
    assert_eq!(json["prepare_id"], PREP);
    assert_eq!(json["hold"], "confirmed");
    assert_eq!(json["blockers"][0]["session"], "w");
    assert_eq!(json["blockers"][0]["display_name"], "name-w");
    assert_eq!(json["rows"][0]["state"], "paused");
    assert_eq!(json["rows"][1]["state"], "working");
    assert_eq!(json["source"], READINESS_SOURCE);
    // Read-only: the record it was computed from is untouched.
    assert_eq!(rec.cohort.rows[0].pause_state, None);
}

#[test]
fn restart_prep_the_notice_is_one_fixed_paragraph() {
    let n = notice_text("abc");
    assert!(!n.contains('\n'), "one paragraph");
    assert!(n.contains("abc"));
    for needed in [
        "commit",
        "WIP",
        "note",
        "background shells or watchers",
        "wait",
        "finished",
    ] {
        assert!(n.contains(needed), "{needed}");
    }
    assert!(!resume_text("abc").contains('\n'));
    assert!(resume_text("abc").contains("cancelled"));
}

#[tokio::test]
async fn restart_prep_admit_prefers_the_idle_arm_and_falls_to_the_busy_one_only_when_not_idle() {
    let submitted = Arc::new(Mutex::new(Vec::new()));
    let idle = FakePane {
        cfg: idle_pane(),
        submitted: submitted.clone(),
    };
    assert_eq!(admit(&idle).await, Ok(InputArm::Idle));
    let busy = FakePane {
        cfg: busy_pane(BUSY_TOOL_CALL),
        submitted: submitted.clone(),
    };
    assert_eq!(admit(&busy).await, Ok(InputArm::BusyPrompt));
    // An idle prompt holding a draft is refused by the idle arm, and the
    // busy arm is NOT tried.
    let mut draft = idle_pane();
    draft.screen.lines[2] = "❯\u{a0}draft".to_string();
    draft.screen.cursor_col = 7;
    let drafted = FakePane {
        cfg: draft,
        submitted: submitted.clone(),
    };
    assert_eq!(admit(&drafted).await, Err(GateMiss::PromptNotEmpty));
    let shell = FakePane {
        cfg: PaneCfg {
            live: false,
            ..idle_pane()
        },
        submitted,
    };
    assert_eq!(admit(&shell).await, Err(GateMiss::ClaudeNotLive));
}

#[tokio::test]
async fn restart_prep_two_records_on_one_pane_get_one_notice() {
    // A transient phantom row: two open records bound to the same terminal.
    let a = row("a");
    let mut b = row("b");
    b.session.terminal_id = "term-a".to_string();
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![a, b], HoldStatus::Confirmed));
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let ev = census(vec![
        ("a", proc(Activity::Idle, Some(false))),
        ("b", proc(Activity::Idle, Some(false))),
    ]);
    let pinned = ctl.record().unwrap().cohort;
    for at in [T0 + 1, T0 + 2] {
        ctl.notice_tick(
            &terms,
            Ok(Cohort {
                captured_at_ms: at,
                ..pinned.clone()
            }),
            &ev,
            at,
        )
        .await;
    }
    assert_eq!(terms.sent("term-a").len(), 1, "one pane, one notice");
    let rec = ctl.record().unwrap();
    assert!(row_of(&rec, "a").notified_at.is_some());
    assert!(row_of(&rec, "b").notified_at.is_some());
    assert_eq!(
        row_of(&rec, "b").notice_arm.as_deref(),
        Some("shared_terminal")
    );
}

#[tokio::test]
async fn restart_prep_a_failed_record_write_never_causes_a_resend() {
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![row("a")], HoldStatus::Confirmed));
    // The record path turns unwritable: a directory where the file was.
    let path = dir.path().join(RECORD_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    for at in [T0 + 1, T0 + 2, T0 + 3] {
        ctl.notice_tick(&terms, recapture(&["a"], at), &ev, at)
            .await;
    }
    assert_eq!(terms.sent("term-a").len(), 1);
    assert_eq!(
        row_of(&ctl.record().unwrap(), "a").notified_at,
        Some(T0 + 1)
    );
    // The disk recovers: the next tick writes the pending change even though
    // nothing new happened, so a restart cannot reload the stale file.
    std::fs::remove_dir(&path).unwrap();
    ctl.notice_tick(&terms, recapture(&["a"], T0 + 4), &ev, T0 + 4)
        .await;
    let on_disk = crate::restart_prep::load_from(&path).expect("written");
    assert_eq!(row_of(&on_disk, "a").notified_at, Some(T0 + 1));
}

#[tokio::test]
async fn restart_prep_a_new_process_on_a_told_pane_is_told_and_an_unmatched_one_waits() {
    // `a` was told on term-a (pid 7). `c` is a different `claude` started in
    // the same shell (pid 8): it was told nothing. `d`'s process is unread.
    let mut a = row("a");
    a.notified_at = Some(T0);
    a.notified_pid = Some(7);
    let mut c = row("c");
    c.session.terminal_id = "term-a".to_string();
    let mut d = row("d");
    d.session.terminal_id = "term-a".to_string();
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![a.clone(), c, d], HoldStatus::Confirmed));
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let mut ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    ev.by_session.get_mut("a").unwrap().pid = 7;
    ev.by_session.insert(
        "c".to_string(),
        ProcEvidence {
            pid: 8,
            ..proc(Activity::Idle, Some(false))
        },
    );
    let pinned = ctl.record().unwrap().cohort;
    ctl.notice_tick(
        &terms,
        Ok(Cohort {
            captured_at_ms: T0 + 1,
            ..pinned
        }),
        &ev,
        T0 + 1,
    )
    .await;
    // c is delivered (its own paste), and d, which then shares a pane told
    // THIS tick, is counted as told with it.
    assert_eq!(terms.sent("term-a").len(), 1);
    let rec = ctl.record().unwrap();
    assert_eq!(row_of(&rec, "c").notice_arm.as_deref(), Some("idle"));
    assert_eq!(row_of(&rec, "c").notified_pid, Some(8));

    // Across ticks: an unidentified process on a pane told earlier waits.
    let mut e = row("e");
    e.session.terminal_id = "term-a".to_string();
    let dir2 = tempfile::tempdir().unwrap();
    let (ctl2, _) = controller(&dir2, &record(vec![a, e], HoldStatus::Confirmed));
    let terms2 = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let mut ev2 = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    ev2.by_session.get_mut("a").unwrap().pid = 7;
    let pinned = ctl2.record().unwrap().cohort;
    ctl2.notice_tick(
        &terms2,
        Ok(Cohort {
            captured_at_ms: T0 + 1,
            ..pinned
        }),
        &ev2,
        T0 + 1,
    )
    .await;
    assert!(terms2.sent("term-a").is_empty(), "never re-sent on a guess");
    let rec = ctl2.record().unwrap();
    assert_eq!(row_of(&rec, "e").notified_at, None);
    assert!(row_of(&rec, "e")
        .notice_deferred
        .as_deref()
        .unwrap()
        .contains("cannot be"));
}

/// A pane whose submit wrote the paste but not the Enter.
struct HalfWritten;

#[async_trait]
impl InputPane for HalfWritten {
    async fn looks_idle_quiescent(&self) -> bool {
        true
    }
    async fn claude_live(&self) -> Result<(), GateMiss> {
        Ok(())
    }
    fn screen(&self) -> ScreenText {
        idle_screen()
    }
}

#[async_trait]
impl NoticePane for HalfWritten {
    fn input(&self) -> &dyn InputPane {
        self
    }
    async fn submit(&self, _: &str) -> Result<(), String> {
        Err(format!(
            "{}: the paste was written but the submit enter failed: broken pipe",
            crate::terminal::session::PROMPT_PARTIALLY_WRITTEN
        ))
    }
}

struct HalfWrittenTerminals(AtomicUsize);

impl NoticeTerminals for HalfWrittenTerminals {
    fn pane(&self, _: &str) -> Option<Box<dyn NoticePane>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(Box::new(HalfWritten))
    }
}

#[tokio::test]
async fn restart_prep_a_paste_left_unsubmitted_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![row("a")], HoldStatus::Confirmed));
    let terms = HalfWrittenTerminals(AtomicUsize::new(0));
    let ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    for at in [T0 + 1, T0 + 2] {
        ctl.notice_tick(&terms, recapture(&["a"], at), &ev, at)
            .await;
    }
    assert_eq!(
        terms.0.load(Ordering::SeqCst),
        1,
        "tried once, then left to the operator"
    );
    let rec = ctl.record().unwrap();
    let a = row_of(&rec, "a");
    assert_eq!(a.notified_at, None);
    assert!(a
        .notice_deferred
        .as_deref()
        .unwrap()
        .starts_with(crate::terminal::session::PROMPT_PARTIALLY_WRITTEN));
}

#[tokio::test]
async fn restart_prep_the_resume_line_skips_rows_that_finished_or_closed() {
    let mut finished = row("f");
    finished.notified_at = Some(T0);
    finished.finished = Some(true);
    let mut closed = row("c");
    closed.notified_at = Some(T0);
    closed.left_capture_at = Some(T0 + 1);
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![finished, closed], HoldStatus::Local));
    let rec = ctl.cancel(&NoCoord, T0 + 10).await.unwrap();
    assert!(!RestartPrep::owes_resume(&rec, T0 + 10));
    let terms = FakeTerminals::with(vec![("term-f", idle_pane()), ("term-c", idle_pane())]);
    assert_eq!(
        ctl.resume_tick(&terms, T0 + 11).await,
        TickOutcome::Idle(PrepState::Cancelled)
    );
    assert!(terms.sent("term-f").is_empty());
    assert!(terms.sent("term-c").is_empty());
}

#[test]
fn restart_prep_a_cloud_hosted_claude_model_is_still_claude() {
    let mut r = row("v");
    r.model = Some("us.anthropic.claude-opus-5-5".to_string());
    assert_eq!(not_claude(&r), None);
}

#[tokio::test]
async fn restart_prep_a_told_session_running_under_a_new_process_is_not_told_again() {
    // `a` was told under pid 7; it now runs as pid 9 (resumed in its pane,
    // carrying the conversation that holds the notice). The pane guard sees a
    // different process; the row's own `notified_at` is what stops a re-send.
    let mut a = row("a");
    a.notified_at = Some(T0);
    a.notified_pid = Some(7);
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![a], HoldStatus::Confirmed));
    let terms = FakeTerminals::with(vec![("term-a", idle_pane())]);
    let mut ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    ev.by_session.get_mut("a").unwrap().pid = 9;
    ctl.notice_tick(&terms, recapture(&["a"], T0 + 1), &ev, T0 + 1)
        .await;
    assert!(terms.sent("term-a").is_empty());
    let rec = ctl.record().unwrap();
    assert_eq!(row_of(&rec, "a").notified_at, Some(T0));
    assert_eq!(row_of(&rec, "a").pause_state, Some(PauseState::Paused));
}

/// The falsified premise (plan Phase 4 results): every `claude` on a box using
/// the stdio coord-mcp shim holds a permanent `python3` child. A told session
/// whose activity reads idle is `paused` anyway, and the verdict can turn
/// ready; the child is reported on the row, never a blocker.
#[tokio::test]
async fn restart_prep_an_idle_told_session_holding_an_mcp_shim_child_is_paused() {
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![row("s")], HoldStatus::Confirmed));
    let terms = FakeTerminals::with(vec![("term-s", idle_pane())]);
    // Idle (from its Claude Code record, the pane arm being vetoed by the
    // child), with the shim attached.
    let shim = census(vec![("s", proc(Activity::Idle, Some(true)))]);
    ctl.notice_tick(&terms, recapture(&["s"], T0 + 1), &shim, T0 + 1)
        .await;
    assert_eq!(terms.sent("term-s").len(), 1, "told");
    let out = ctl
        .notice_tick(&terms, recapture(&["s"], T0 + 2), &shim, T0 + 2)
        .await;
    assert!(
        matches!(
            out,
            TickOutcome::Ran {
                ready_to_restart: true,
                ..
            }
        ),
        "{out:?}"
    );
    let rec = ctl.record().unwrap();
    let s_row = row_of(&rec, "s");
    assert_eq!(s_row.pause_state, Some(PauseState::Paused));
    assert_eq!(
        s_row.has_live_children,
        Some(true),
        "reported, not required"
    );
    assert_eq!(rec.state, PrepState::Ready);
    let block = evaluate_fresh(&rec, &shim, T0 + 3);
    assert!(
        block.verdict.ready_to_restart,
        "{:?}",
        block.verdict.blockers
    );
    let json = serde_json::to_value(&block).unwrap();
    assert_eq!(json["rows"][0]["has_live_children"], true);
    // A foreground tool call still reads working, and is not paused.
    let tool_call = census(vec![("s", proc(Activity::Working, Some(true)))]);
    assert!(
        !evaluate_fresh(&rec, &tool_call, T0 + 4)
            .verdict
            .ready_to_restart
    );
}

// ---------------------------------------------------------------------------
// The held toggle vs a running tick (Phase 5)
// ---------------------------------------------------------------------------

/// A pane whose paste blocks until released, so a tick can be held at the
/// point where it has read the cohort and not yet written it back.
struct GatedTerminals {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

struct GatedPane {
    cfg: PaneCfg,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl InputPane for GatedPane {
    async fn looks_idle_quiescent(&self) -> bool {
        self.cfg.quiescent
    }
    async fn claude_live(&self) -> Result<(), GateMiss> {
        Ok(())
    }
    fn screen(&self) -> ScreenText {
        self.cfg.screen.clone()
    }
}

#[async_trait]
impl NoticePane for GatedPane {
    fn input(&self) -> &dyn InputPane {
        self
    }
    async fn submit(&self, _text: &str) -> Result<(), String> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

impl NoticeTerminals for GatedTerminals {
    fn pane(&self, _terminal_id: &str) -> Option<Box<dyn NoticePane>> {
        Some(Box::new(GatedPane {
            cfg: idle_pane(),
            entered: self.entered.clone(),
            release: self.release.clone(),
        }))
    }
}

/// The tick replaces the whole cohort with the one it read at its start; a
/// held toggle written while it ran would be lost. `set_held_on` takes the
/// same `ops` lock, so the toggle lands after the tick, on its result.
#[tokio::test]
async fn restart_prep_a_held_toggle_during_a_tick_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let (ctl, _) = controller(&dir, &record(vec![row("a")], HoldStatus::Confirmed));
    let terms = GatedTerminals {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    let ev = census(vec![("a", proc(Activity::Idle, Some(false)))]);
    let toggle = async {
        terms.entered.notified().await;
        let set = crate::restart_prep::restore::set_held_on(&ctl, PREP, "a", true);
        tokio::pin!(set);
        // Let the toggle run as far as it can while the tick is mid-paste.
        let early = tokio::time::timeout(std::time::Duration::from_millis(100), &mut set).await;
        terms.release.notify_one();
        match early {
            Ok(r) => r,
            Err(_) => set.await,
        }
    };
    let (tick, held) = tokio::join!(
        ctl.notice_tick(&terms, recapture(&["a"], T0 + 1_000), &ev, T0 + 1_000),
        toggle
    );
    assert!(
        matches!(tick, TickOutcome::Ran { delivered: 1, .. }),
        "{tick:?}"
    );
    held.unwrap();
    let rec = ctl.record().unwrap();
    let a = row_of(&rec, "a");
    assert!(a.held, "the toggle survived the tick's write");
    assert_eq!(
        a.notified_at,
        Some(T0 + 1_000),
        "and the tick's own stamp too"
    );
}
