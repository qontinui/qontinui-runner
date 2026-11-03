//! Does a pane run the session its lifecycle row names?
//!
//! Plan `2026-10-31-runner-binds-a-terminal-to-the-wrong-live-session`
//! (Phases 2 and 3).
//!
//! # Why
//!
//! A lifecycle row is keyed by `claude_session_id` and records the
//! `terminal_id` it lives on. Every binder — the spawn pin, the provider hook,
//! the typed-resume sniff, reconcile's rungs, the transcript-mtime fallbacks —
//! writes that pair through
//! [`SessionLifecycleStore::record_open`](crate::session::session_lifecycle_store::SessionLifecycleStore::record_open),
//! and until this module nothing checked the pair against the pane itself. On
//! 2026-10-25 (merytshost) reconcile rung 3 bound a session that was open —
//! never hook-confirmed — on a live peer pane to a second pane; `record_open`
//! moved the session's row there and its supersede scan evicted the second
//! pane's own session. One pane wore another session's labels, the other wore
//! none, and every consumer that labels, nudges or closes a session by terminal
//! acted on the wrong one.
//!
//! # The evidence
//!
//! Claude Code writes `<config_dir>/sessions/<pid>.json` for each running
//! interactive process, naming the `sessionId` that process runs NOW. It
//! follows `/clear` and an in-TUI resume, where the argv id and the spawn pin
//! stay fixed at launch. It is read through the existing reader,
//! [`claude_activity::read_records`], which already rejects a record left by an
//! earlier holder of the pid and a pid two config dirs disagree about. Only a
//! pane's TOP-LEVEL `claude` processes count ([`top_level_claude`]): a nested
//! one (a `claude` a session launched) runs a session of its own and says
//! nothing about the pane's.
//!
//! A missing, unparseable, ambiguous or reused-pid record — or one naming no
//! session — is never read as agreement: the pane is then
//! [`PaneEvidence::Unknown`].
//!
//! # Who reads it
//!
//! - **The reconcile pass** (every liveness tick,
//!   [`crate::session::reconcile::run_reconcile_pass`]) reads every live pane's
//!   evidence, binds an unconfirmed pane from it (the provider-registry rung),
//!   then compares each pane's open rows with it ([`binding_verdicts`]) and
//!   stores the verdict as the row's
//!   [`BindingState`](crate::session::session_lifecycle_store::BindingState).
//!   A row that newly reads `unknown` is logged (`session binder: binding
//!   mismatch`) and counted ([`binding_mismatch_total`], served as `/health`
//!   `sessionTracking.bindingMismatchTotal`).
//! - **`record_open` and `rebind_terminal`** ask the attached
//!   [`PaneEvidenceOracle`] before moving a row off another terminal: a pane
//!   that still runs that session keeps it ([`binding_move_refused_total`]).
//! - **The tracking-health census** attributes each terminal-hosted `claude`
//!   pid from its own record, so `/restart-readiness` stops echoing a row's id
//!   for a process that records another.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tracing::warn;

use crate::process_capture::process_tree::{claude_pids_in_inclusive_subtree, ProcessSnapshot};
use crate::session::claude_activity::{self, LivePid, RecordReading, TranscriptTail};
use crate::session::session_lifecycle_store::{
    BindingState, SessionLifecycleStore, TerminalSessionRecord,
};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

/// What a pane's own `claude` process records about the session it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvidence {
    /// No `claude` runs in the pane — or the pane is not one this runner saw
    /// live. Nothing there runs a session.
    NoClaude,
    /// Every top-level `claude` in the pane records this session id.
    Names(String),
    /// A `claude` runs in the pane, but which session it runs cannot be
    /// established: a record is missing, unparseable, ambiguous, left by an
    /// earlier holder of the pid or names no session, or two top-level
    /// processes name different sessions. Never read as agreement.
    Unknown,
}

impl PaneEvidence {
    /// The session the pane positively runs, if its evidence names one.
    pub fn named_session(&self) -> Option<&str> {
        match self {
            PaneEvidence::Names(id) => Some(id),
            PaneEvidence::NoClaude | PaneEvidence::Unknown => None,
        }
    }
}

/// The pane's TOP-LEVEL `claude` processes — those with no `claude` ancestor
/// inside the pane rooted at `root_pid` — each carrying the start time the
/// snapshot implies (epoch seconds → millis), which is what
/// [`claude_activity::read_records`] checks a record's `startedAt` against
/// where `/proc` cannot answer. Breadth-first from the root, so the order is
/// stable across passes over an unchanged process table.
pub fn top_level_claude(root_pid: u32, snapshot: &ProcessSnapshot) -> Vec<LivePid> {
    let claude: HashSet<u32> = claude_pids_in_inclusive_subtree(root_pid, snapshot)
        .into_iter()
        .collect();
    if claude.is_empty() {
        return Vec::new();
    }
    let mut top = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut queue: VecDeque<(u32, bool)> = VecDeque::from([(root_pid, false)]);
    while let Some((pid, under_claude)) = queue.pop_front() {
        if !seen.insert(pid) {
            continue;
        }
        let is_claude = claude.contains(&pid);
        if is_claude && !under_claude {
            top.push(LivePid {
                pid,
                process_started_ms: snapshot
                    .creation_times
                    .get(&pid)
                    .copied()
                    .filter(|secs| *secs > 0)
                    .map(|secs| secs * 1000),
            });
        }
        if let Some(kids) = snapshot.parent_map.get(&pid) {
            for &kid in kids {
                queue.push_back((kid, under_claude || is_claude));
            }
        }
    }
    top
}

/// One pane's evidence from its top-level `claude` pids and their records.
/// Pure. Every pid must hold a parsed record naming a session, and they must
/// all name the SAME one; anything less is [`PaneEvidence::Unknown`].
pub fn evidence_from(pids: &[LivePid], records: &HashMap<u32, RecordReading>) -> PaneEvidence {
    if pids.is_empty() {
        return PaneEvidence::NoClaude;
    }
    let mut named: Option<&str> = None;
    for live in pids {
        let Some(RecordReading::Parsed(ev)) = records.get(&live.pid) else {
            return PaneEvidence::Unknown;
        };
        let Some(id) = ev.session_id.as_deref() else {
            return PaneEvidence::Unknown;
        };
        match named {
            None => named = Some(id),
            Some(prev) if prev == id => {}
            Some(_) => return PaneEvidence::Unknown,
        }
    }
    named.map_or(PaneEvidence::Unknown, |id| {
        PaneEvidence::Names(id.to_string())
    })
}

/// How long one pass — a reconcile pass, or a tracking-health census such as
/// `/restart-readiness` runs — may spend reading the processes' records. The
/// same 3 s `/restart-readiness` already allows its activity read, since that
/// endpoint answers under a short client timeout. A read that overruns leaves
/// every pane `Unknown` for that pass: no verdict moves on absent evidence.
pub const RECORD_READ_BOUND: Duration = Duration::from_secs(3);

/// Read Claude Code's records for `pids` on the BLOCKING pool, without the
/// transcript tail (only `sessionId` is wanted), under `bound` — measured off
/// the runtime ([`crate::off_runtime::deadline`]) for the reason
/// `reconcile::PrescannedTranscriptIndex::spawn_scan` gives. A read that fails
/// or overruns returns NO readings, so every pane reads `Unknown`.
pub async fn read_records_off_runtime(
    pids: Vec<LivePid>,
    bound: Duration,
) -> HashMap<u32, RecordReading> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let deadline = crate::off_runtime::deadline(bound);
    let task = spawn_blocking_tracked(move || {
        claude_activity::read_records(
            &crate::terminal::transcript::find_claude_config_dirs(),
            &pids,
            &claude_activity::proc_start_ticks,
            TranscriptTail::Skip,
        )
    });
    tokio::select! {
        joined = task => joined.unwrap_or_else(|e| {
            warn!(
                error = %e,
                "session binder: the pane record read failed — every pane reads unknown this pass"
            );
            HashMap::new()
        }),
        _ = deadline => {
            warn!(
                bound_secs = bound.as_secs(),
                "session binder: the pane record read did not finish within its bound — every \
                 pane reads unknown this pass"
            );
            HashMap::new()
        }
    }
}

// ---------------------------------------------------------------------------
// The store's oracle
// ---------------------------------------------------------------------------

/// The store's view of the panes' own evidence, attached once at startup
/// ([`SessionLifecycleStore::attach_pane_evidence`]). The store never reads a
/// process table or a file itself; this is the whole of what it knows about a
/// pane, which is what keeps it unit-testable.
pub trait PaneEvidenceOracle: Send + Sync + std::fmt::Debug {
    /// What `terminal_id`'s pane records NOW.
    fn pane_evidence(&self, terminal_id: &str) -> PaneEvidence;

    /// Each reconcile pass hands over every live pane's top-level `claude`
    /// processes (an empty list for a pane with none), so a later query knows
    /// which pids to re-read. A pane absent from `panes` was not live.
    fn observe_panes(&self, _panes: &HashMap<String, Vec<LivePid>>) {}
}

/// The production oracle: each live pane's top-level `claude` pids as the last
/// reconcile pass saw them, RE-READ on every query — a process that exited,
/// lost its pid to another, or switched sessions since that pass cannot hold a
/// row on stale evidence.
///
/// - a pane the last pass did not see live, or saw with no `claude`, or whose
///   `claude` has since exited → [`PaneEvidence::NoClaude`];
/// - otherwise the fresh record read decides ([`evidence_from`]).
///
/// A `claude` started in a pane AFTER the last pass is invisible until the next
/// one (≤ one liveness tick); such a pane answers `NoClaude`, which never
/// refuses a move, so the gap fails open to the pre-guard behaviour.
#[derive(Debug, Default)]
pub struct LivePaneRegistry {
    panes: Mutex<HashMap<String, Vec<LivePid>>>,
}

impl LivePaneRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PaneEvidenceOracle for LivePaneRegistry {
    fn pane_evidence(&self, terminal_id: &str) -> PaneEvidence {
        let pids = match self.panes.lock() {
            Ok(panes) => panes.get(terminal_id).cloned().unwrap_or_default(),
            Err(e) => {
                warn!(error = %e, "session binder: pane registry lock poisoned — pane reads unknown");
                return PaneEvidence::Unknown;
            }
        };
        let alive: Vec<LivePid> = pids
            .into_iter()
            .filter(|p| crate::process_capture::health::pid_alive(p.pid))
            .collect();
        if alive.is_empty() {
            return PaneEvidence::NoClaude;
        }
        let records = claude_activity::read_records(
            &crate::terminal::transcript::find_claude_config_dirs(),
            &alive,
            &claude_activity::proc_start_ticks,
            TranscriptTail::Skip,
        );
        evidence_from(&alive, &records)
    }

    fn observe_panes(&self, panes: &HashMap<String, Vec<LivePid>>) {
        match self.panes.lock() {
            Ok(mut held) => *held = panes.clone(),
            Err(e) => {
                warn!(error = %e, "session binder: pane registry lock poisoned — not updated")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Verdicts and counters
// ---------------------------------------------------------------------------

static BINDING_MISMATCH_TOTAL: AtomicU64 = AtomicU64::new(0);
static BINDING_MOVE_REFUSED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// `binding_mismatch_total` — open rows this process found newly disagreeing
/// with their pane's own `claude` record: each transition INTO
/// [`BindingState::Unknown`], so a mismatch that persists is counted once.
/// Served as `/health` `sessionTracking.bindingMismatchTotal`.
pub fn binding_mismatch_total() -> u64 {
    BINDING_MISMATCH_TOTAL.load(Ordering::Relaxed)
}

/// `binding_move_refused_total` — binds the store refused because they would
/// have moved a row off a live pane that still runs that session. Served as
/// `/health` `sessionTracking.bindingMoveRefusedTotal`.
pub fn binding_move_refused_total() -> u64 {
    BINDING_MOVE_REFUSED_TOTAL.load(Ordering::Relaxed)
}

/// One row newly read [`BindingState::Unknown`]: the warn line and the
/// counter. `evidenced` is the session the pane's own record names (`None`
/// when the pane could not vouch for anything); `context` says which check
/// found it (`reconcile`, `bind`, `refused-move`).
pub(crate) fn note_binding_mismatch(
    terminal_id: &str,
    bound_session: &str,
    evidenced_session: Option<&str>,
    context: &str,
) {
    BINDING_MISMATCH_TOTAL.fetch_add(1, Ordering::Relaxed);
    warn!(
        terminal_id,
        bound_session,
        evidenced_session = evidenced_session.unwrap_or("<unknown>"),
        context,
        "session binder: binding mismatch"
    );
}

/// The store refused a move. See [`binding_move_refused_total`].
pub(crate) fn note_move_refused(session: &str, held_by: &str, refused_terminal: &str) {
    BINDING_MOVE_REFUSED_TOTAL.fetch_add(1, Ordering::Relaxed);
    warn!(
        claude_session = session,
        held_by,
        refused_terminal,
        "session binder: refused to move a session's row off a live pane that still runs it"
    );
}

/// One open row's verdict against its pane's evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingVerdict {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub state: BindingState,
    /// The session the pane's own record names.
    pub evidenced: String,
}

/// Pure: a verdict for every open row on a pane whose evidence NAMES a
/// session — `Matched` for the named one, `Unknown` for any other. A row on a
/// pane with no `claude`, unreadable evidence, or no entry in `evidence` gets
/// no verdict and keeps the state it last read.
pub fn binding_verdicts(
    open: &[TerminalSessionRecord],
    evidence: &HashMap<String, PaneEvidence>,
) -> Vec<BindingVerdict> {
    open.iter()
        .filter(|r| r.state == "open")
        .filter_map(|r| {
            let evidenced = evidence.get(&r.terminal_id)?.named_session()?;
            Some(BindingVerdict {
                claude_session_id: r.claude_session_id.clone(),
                terminal_id: r.terminal_id.clone(),
                state: if evidenced == r.claude_session_id {
                    BindingState::Matched
                } else {
                    BindingState::Unknown
                },
                evidenced: evidenced.to_string(),
            })
        })
        .collect()
}

/// Write `verdicts` to the store, and log + count every row that NEWLY reads
/// `Unknown`. Returns those rows. A verdict for a row that moved or closed
/// since the verdict was taken is dropped by the store.
pub fn apply_binding_verdicts(
    store: &SessionLifecycleStore,
    verdicts: &[BindingVerdict],
) -> Vec<BindingVerdict> {
    let mut newly_unknown = Vec::new();
    for v in verdicts {
        let Some(previous) = store.set_binding_state(&v.claude_session_id, &v.terminal_id, v.state)
        else {
            continue;
        };
        if v.state == BindingState::Unknown && previous != Some(BindingState::Unknown) {
            note_binding_mismatch(
                &v.terminal_id,
                &v.claude_session_id,
                Some(&v.evidenced),
                "reconcile",
            );
            newly_unknown.push(v.clone());
        }
    }
    newly_unknown
}

/// A fixed oracle for tests across the session modules: each listed terminal
/// answers its evidence, and any other terminal is not a live pane.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FixedPanes(pub HashMap<String, PaneEvidence>);

#[cfg(test)]
impl FixedPanes {
    pub(crate) fn of(panes: &[(&str, PaneEvidence)]) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(
            panes
                .iter()
                .map(|(terminal, evidence)| (terminal.to_string(), evidence.clone()))
                .collect(),
        ))
    }
}

#[cfg(test)]
impl PaneEvidenceOracle for FixedPanes {
    fn pane_evidence(&self, terminal_id: &str) -> PaneEvidence {
        self.0
            .get(terminal_id)
            .cloned()
            .unwrap_or(PaneEvidence::NoClaude)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::claude_activity::RecordEvidence;

    fn parsed(session: Option<&str>) -> RecordReading {
        RecordReading::Parsed(RecordEvidence {
            status: "idle".to_string(),
            status_updated_at_ms: None,
            last_message_ms: None,
            session_id: session.map(str::to_string),
        })
    }

    fn live(pid: u32) -> LivePid {
        LivePid {
            pid,
            process_started_ms: None,
        }
    }

    /// shell 10 → claude 11 → bash 12 → claude 13 (a nested `claude -p`), and
    /// a second top-level claude 14 under the shell. Image names are what
    /// `claude_pids_in_inclusive_subtree` classifies.
    fn pane_snapshot() -> ProcessSnapshot {
        let mut s = ProcessSnapshot::default();
        s.parent_map.insert(10, vec![11, 14]);
        s.parent_map.insert(11, vec![12]);
        s.parent_map.insert(12, vec![13]);
        for (pid, name) in [
            (10, "bash"),
            (11, "claude"),
            (12, "bash"),
            (13, "claude"),
            (14, "claude"),
        ] {
            s.names.insert(pid, name.to_string());
            s.creation_times.insert(pid, 1_000 + i64::from(pid));
        }
        s
    }

    #[test]
    fn top_level_claude_excludes_a_claude_nested_under_another() {
        let top = top_level_claude(10, &pane_snapshot());
        let pids: Vec<u32> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![11, 14], "claude 13 runs under claude 11");
        assert_eq!(
            top[0].process_started_ms,
            Some(1_011_000),
            "the start time rides along, in millis"
        );
        assert!(top_level_claude(12, &pane_snapshot())
            .iter()
            .all(|p| p.pid == 13));
        let mut no_claude = ProcessSnapshot::default();
        no_claude.names.insert(1, "bash".to_string());
        assert!(top_level_claude(1, &no_claude).is_empty());
    }

    #[test]
    fn evidence_names_a_session_only_when_every_top_level_record_agrees() {
        let records: HashMap<u32, RecordReading> = [
            (1, parsed(Some("s-1"))),
            (2, parsed(Some("s-1"))),
            (3, parsed(Some("s-3"))),
            (4, parsed(None)),
            (5, RecordReading::Missing),
            (6, RecordReading::Unparseable),
            (7, RecordReading::Ambiguous),
            (8, RecordReading::PidReused),
        ]
        .into_iter()
        .collect();
        assert_eq!(evidence_from(&[], &records), PaneEvidence::NoClaude);
        assert_eq!(
            evidence_from(&[live(1), live(2)], &records),
            PaneEvidence::Names("s-1".to_string())
        );
        assert_eq!(
            evidence_from(&[live(1), live(3)], &records),
            PaneEvidence::Unknown,
            "two top-level processes naming different sessions"
        );
        // A record that cannot be read is never agreement — not even beside
        // one that names the session.
        for unreadable in [4, 5, 6, 7, 8, 99] {
            assert_eq!(
                evidence_from(&[live(1), live(unreadable)], &records),
                PaneEvidence::Unknown,
                "pid {unreadable}"
            );
        }
    }

    fn open_row(id: &str, terminal: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            terminal_id: terminal.to_string(),
            ..crate::session::session_lifecycle_store::test_open_record(id)
        }
    }

    #[test]
    fn verdicts_cover_only_panes_whose_evidence_names_a_session() {
        let rows = vec![
            open_row("s-a", "t-a"),
            open_row("phantom", "t-a"),
            open_row("s-b", "t-b"),
            open_row("s-c", "t-c"),
            open_row("s-d", "t-gone"),
        ];
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a".to_string(), PaneEvidence::Names("s-a".to_string())),
            ("t-b".to_string(), PaneEvidence::Unknown),
            ("t-c".to_string(), PaneEvidence::NoClaude),
        ]
        .into_iter()
        .collect();
        let verdicts = binding_verdicts(&rows, &evidence);
        let by_id: HashMap<&str, BindingState> = verdicts
            .iter()
            .map(|v| (v.claude_session_id.as_str(), v.state))
            .collect();
        assert_eq!(by_id.get("s-a"), Some(&BindingState::Matched));
        assert_eq!(by_id.get("phantom"), Some(&BindingState::Unknown));
        assert_eq!(
            by_id.len(),
            2,
            "no verdict without a named session: {verdicts:?}"
        );
    }

    #[test]
    fn applying_verdicts_counts_each_new_mismatch_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        store.record_open(open_row("s-b", "t-a"));
        let evidence: HashMap<String, PaneEvidence> =
            [("t-a".to_string(), PaneEvidence::Names("s-a".to_string()))]
                .into_iter()
                .collect();
        let verdicts = binding_verdicts(&store.open_records(), &evidence);

        let first = apply_binding_verdicts(&store, &verdicts);
        assert_eq!(first.len(), 1, "a new mismatch is reported");
        assert_eq!(
            store.get("s-b").unwrap().binding_state,
            Some(BindingState::Unknown)
        );
        let again = apply_binding_verdicts(&store, &verdicts);
        assert!(again.is_empty(), "a persisting mismatch is not re-reported");

        // The pane comes to run the row's session: the verdict clears.
        let agree: HashMap<String, PaneEvidence> =
            [("t-a".to_string(), PaneEvidence::Names("s-b".to_string()))]
                .into_iter()
                .collect();
        apply_binding_verdicts(&store, &binding_verdicts(&store.open_records(), &agree));
        assert_eq!(
            store.get("s-b").unwrap().binding_state,
            Some(BindingState::Matched)
        );
    }

    #[test]
    fn a_registry_with_no_observed_pane_never_names_a_session() {
        let registry = LivePaneRegistry::new();
        assert_eq!(registry.pane_evidence("never-seen"), PaneEvidence::NoClaude);
        // A pane observed with no claude.
        registry.observe_panes(&[("t-empty".to_string(), Vec::new())].into_iter().collect());
        assert_eq!(registry.pane_evidence("t-empty"), PaneEvidence::NoClaude);
        // A pane whose observed claude has exited (pid 0 is never alive).
        registry.observe_panes(
            &[("t-exited".to_string(), vec![live(0)])]
                .into_iter()
                .collect(),
        );
        assert_eq!(registry.pane_evidence("t-exited"), PaneEvidence::NoClaude);
    }
}
