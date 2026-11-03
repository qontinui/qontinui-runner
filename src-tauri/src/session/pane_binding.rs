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
//! [`SessionLifecycleStore::record_open_checked`](crate::session::session_lifecycle_store::SessionLifecycleStore::record_open_checked),
//! and until this module nothing checked the pair against the pane itself. On
//! 2026-10-25 (merytshost) one pane's lifecycle row came to name the session a
//! LIVE PEER pane was running, and the first pane's own session row was
//! closed: one pane wore another session's labels, the other wore none, and
//! every consumer that labels, nudges or closes a session by terminal acted on
//! the wrong one. WHICH binder made that move is UNKNOWN until merytshost's
//! runner logs for that day are read. Reconcile rung 3 is one binder that
//! produces exactly that shape: the Phase 1 test
//! (`reconcile::tests::rung3_never_moves_a_live_peers_unconfirmed_session_onto_another_pane`)
//! fails on a clean `fd646a5be` with only the test added.
//!
//! # The evidence
//!
//! Claude Code writes `<config_dir>/sessions/<pid>.json` for each running
//! interactive process, naming a `sessionId`. It is read through the existing
//! reader, [`claude_activity::read_records`], which already rejects a record
//! left by an earlier holder of the pid and a pid two config dirs disagree
//! about. Only a pane's TOP-LEVEL `claude` processes count
//! ([`top_level_claude`]): a nested one (a `claude` a session launched) runs a
//! session of its own and says nothing about the pane's.
//!
//! A missing, unparseable, ambiguous or reused-pid record — or one naming no
//! session — is never read as agreement: the pane is then
//! [`PaneEvidence::Unknown`].
//!
//! **An assumption, UNMEASURED.** That the record's `sessionId` follows the
//! process when `/clear` or an in-TUI `/resume` switches it to another session
//! is assumed, not measured. Read-only inspection of every
//! `~/.claude*/sessions/*.json` on nawa on 2026-11-03 found 142 live records:
//! 139 carried a session id in their process's argv and all 139 equalled the
//! record's `sessionId`; none of those processes had switched sessions, so the
//! sample says nothing either way. The verdicts are therefore built so a record
//! that LAGS a switch the provider's hook already reported never reads the
//! newer row `unknown` ([`binding_verdicts`]).
//!
//! # Where it is read, and who reads the result
//!
//! - **The reconcile pass** (every liveness tick,
//!   [`crate::session::reconcile::run_reconcile_pass`]) reads every live pane's
//!   top-level records ONCE, on the blocking pool under [`RECORD_READ_BOUND`],
//!   and hands the result to the store's oracle as a [`PaneObservation`]. It
//!   binds an unbound pane from it (the provider-registry rung), then judges
//!   each live pane's open rows against it ([`binding_verdicts`]) and stores
//!   the verdict as the row's
//!   [`BindingState`](crate::session::session_lifecycle_store::BindingState).
//! - **`record_open_checked` and `rebind_terminal_checked`** ask the attached
//!   [`PaneEvidenceOracle`] before moving a row off another terminal: a pane
//!   that still runs that session keeps it. [`LivePaneRegistry`] answers from
//!   the pass's observation — no file I/O, only a liveness probe per pid — so a
//!   bind on a tokio worker or the Tauri main thread never waits on a disk.
//! - **The tracking-health census** (`/restart-readiness`) attributes each
//!   terminal-hosted `claude` pid from the same observation
//!   ([`PaneEvidenceOracle::recorded_sessions`]), so the census never reads a
//!   file on its verdict path either.
//!
//! # What is counted
//!
//! [`BindingCounters`]: a row that turns `unknown` because a pane's own record
//! NAMES another session — once per transition, and not when the row is that
//! pane's prior tenant (a `/clear` or an in-TUI resume the hook already
//! recorded); and a refused move — once per (session, pane) episode, not once
//! per retry. Served as `/health` `sessionTracking.bindingMismatchTotal` and
//! `bindingMoveRefusedTotal`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

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
    /// earlier holder of the pid or names no session, two top-level processes
    /// name different sessions, or the evidence is too old to stand on. Never
    /// read as agreement.
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

/// The session each pid's record NAMES, from a read's readings: a parsed
/// record (the pid's own, not an earlier holder's) with a non-blank
/// `sessionId`. Every other reading is absent here — absent is never agreement.
pub fn sessions_from_readings(readings: &HashMap<u32, RecordReading>) -> HashMap<u32, String> {
    readings
        .iter()
        .filter_map(|(pid, reading)| match reading {
            RecordReading::Parsed(ev) => ev.session_id.clone().map(|id| (*pid, id)),
            _ => None,
        })
        .collect()
}

/// One pane's evidence from its top-level `claude` pids and the session each
/// pid's record names. Pure. Every pid must name a session, and they must all
/// name the SAME one; anything less is [`PaneEvidence::Unknown`].
pub fn evidence_from(pids: &[LivePid], sessions: &HashMap<u32, String>) -> PaneEvidence {
    if pids.is_empty() {
        return PaneEvidence::NoClaude;
    }
    let mut named: Option<&str> = None;
    for live in pids {
        let Some(id) = sessions.get(&live.pid) else {
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

/// How long one reconcile pass may spend reading the panes' records. A read
/// that overruns leaves every pane `Unknown` for that pass: no verdict moves on
/// absent evidence.
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
// One pass's observation, and the store's oracle over it
// ---------------------------------------------------------------------------

/// What one reconcile pass saw: every live pane's top-level `claude` pids (an
/// empty list for a live pane with no `claude`), and the session each pid's
/// own record names. A pane absent from `panes` was not live; a pid absent
/// from `sessions` has no readable record naming a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneObservation {
    pub panes: HashMap<String, Vec<LivePid>>,
    pub sessions: HashMap<u32, String>,
}

impl PaneObservation {
    /// Each observed pane's evidence, as the pass saw it.
    pub fn evidence_map(&self) -> HashMap<String, PaneEvidence> {
        self.panes
            .iter()
            .map(|(terminal, pids)| (terminal.clone(), evidence_from(pids, &self.sessions)))
            .collect()
    }
}

/// One pid's record as the latest pass read it: the start time the pass's
/// snapshot gave the pid (so a reader can tell a pid that was reused since),
/// and the session the record names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSession {
    pub process_started_ms: Option<i64>,
    pub session_id: String,
}

/// The store's view of the panes' own evidence, attached once at startup
/// ([`SessionLifecycleStore::attach_pane_evidence`]). The store never reads a
/// process table or a file itself; this is the whole of what it knows about a
/// pane, which is what keeps it unit-testable. Every method answers from
/// memory (plus, in production, a liveness probe per pid): the store asks it
/// while holding its registry lock.
pub trait PaneEvidenceOracle: Send + Sync + std::fmt::Debug {
    /// What `terminal_id`'s pane records NOW.
    fn pane_evidence(&self, terminal_id: &str) -> PaneEvidence;

    /// Every live pane whose own record names `session_id`.
    fn panes_naming(&self, session_id: &str) -> Vec<String>;

    /// pid → its record, from the latest observation — `None` when there is no
    /// observation fresh enough to stand on.
    fn recorded_sessions(&self) -> Option<HashMap<u32, RecordedSession>> {
        None
    }

    /// Each reconcile pass hands over what it saw.
    fn observe(&self, _observation: PaneObservation) {}
}

/// How old the latest observation may be before nothing is concluded from it:
/// three liveness ticks (the poll runs every 45 s), so one slow or skipped pass
/// never blinds the guard, but a stalled poll is not answered from a picture of
/// panes that may have changed since.
pub const OBSERVATION_MAX_AGE: Duration = Duration::from_secs(135);

/// The production oracle: the latest reconcile pass's [`PaneObservation`],
/// answered from memory. Each pid is re-checked for liveness on every query,
/// so a `claude` that has exited since the pass cannot hold a row there:
///
/// - nothing observed yet, a pane the pass did not see live, a pane it saw
///   with no `claude`, or one whose `claude` has since exited →
///   [`PaneEvidence::NoClaude`];
/// - an observation older than its max age ([`OBSERVATION_MAX_AGE`]) →
///   [`PaneEvidence::Unknown`];
/// - otherwise the pass's records for the pids still alive decide
///   ([`evidence_from`]).
///
/// A `claude` started in a pane AFTER the last pass is invisible until the next
/// one (≤ one liveness tick); such a pane answers `NoClaude`, which never
/// refuses a move, so the gap fails open to the pre-guard behaviour. A pid
/// reused within that window would keep the old process's record — and every
/// use of `Names` is conservative (it keeps a row where it is, or spares it
/// from a supersede), so the error is bounded to keeping a row one tick longer.
#[derive(Debug)]
pub struct LivePaneRegistry {
    latest: Mutex<Option<(PaneObservation, Instant)>>,
    max_age: Duration,
}

impl Default for LivePaneRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LivePaneRegistry {
    pub fn new() -> Self {
        Self::with_max_age(OBSERVATION_MAX_AGE)
    }

    /// A registry whose observations go stale after `max_age`.
    pub fn with_max_age(max_age: Duration) -> Self {
        Self {
            latest: Mutex::new(None),
            max_age,
        }
    }

    /// The latest observation, `Err(())` when it is too old to stand on (or the
    /// lock is poisoned) and `Ok(None)` when nothing was observed yet.
    fn fresh(&self) -> Result<Option<PaneObservation>, ()> {
        let held = match self.latest.lock() {
            Ok(held) => held,
            Err(e) => {
                warn!(error = %e, "session binder: pane registry lock poisoned — pane reads unknown");
                return Err(());
            }
        };
        match held.as_ref() {
            None => Ok(None),
            Some((_, at)) if at.elapsed() > self.max_age => Err(()),
            Some((observation, _)) => Ok(Some(observation.clone())),
        }
    }

    /// The evidence of one pane of `observation`, over its pids still alive.
    fn live_evidence(observation: &PaneObservation, terminal_id: &str) -> PaneEvidence {
        let Some(pids) = observation.panes.get(terminal_id) else {
            return PaneEvidence::NoClaude;
        };
        let alive: Vec<LivePid> = pids
            .iter()
            .copied()
            .filter(|p| crate::process_capture::health::pid_alive(p.pid))
            .collect();
        evidence_from(&alive, &observation.sessions)
    }
}

impl PaneEvidenceOracle for LivePaneRegistry {
    fn pane_evidence(&self, terminal_id: &str) -> PaneEvidence {
        match self.fresh() {
            Err(()) => PaneEvidence::Unknown,
            Ok(None) => PaneEvidence::NoClaude,
            Ok(Some(observation)) => Self::live_evidence(&observation, terminal_id),
        }
    }

    fn panes_naming(&self, session_id: &str) -> Vec<String> {
        let Ok(Some(observation)) = self.fresh() else {
            return Vec::new();
        };
        let mut terminals: Vec<String> = observation
            .panes
            .keys()
            .filter(|terminal| {
                Self::live_evidence(&observation, terminal).named_session() == Some(session_id)
            })
            .cloned()
            .collect();
        terminals.sort();
        terminals
    }

    fn recorded_sessions(&self) -> Option<HashMap<u32, RecordedSession>> {
        let observation = self.fresh().ok()??;
        let started: HashMap<u32, Option<i64>> = observation
            .panes
            .values()
            .flatten()
            .map(|p| (p.pid, p.process_started_ms))
            .collect();
        Some(
            observation
                .sessions
                .iter()
                .map(|(pid, session)| {
                    (
                        *pid,
                        RecordedSession {
                            process_started_ms: started.get(pid).copied().flatten(),
                            session_id: session.clone(),
                        },
                    )
                })
                .collect(),
        )
    }

    fn observe(&self, observation: PaneObservation) {
        match self.latest.lock() {
            Ok(mut held) => *held = Some((observation, Instant::now())),
            Err(e) => {
                warn!(error = %e, "session binder: pane registry lock poisoned — not updated")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

/// The pane-binding counters. [`BindingCounters::global`] is the instance
/// `/health` serves and every production store counts into; a test gives its
/// store its own
/// ([`SessionLifecycleStore::attach_binding_counters`]) so it can assert an
/// exact count while other tests run in the same process.
#[derive(Debug, Default)]
pub struct BindingCounters {
    mismatches: AtomicU64,
    refused_moves: AtomicU64,
}

static GLOBAL_COUNTERS: BindingCounters = BindingCounters {
    mismatches: AtomicU64::new(0),
    refused_moves: AtomicU64::new(0),
};

impl BindingCounters {
    /// The process-wide counters `/health` serves.
    pub fn global() -> &'static BindingCounters {
        &GLOBAL_COUNTERS
    }

    /// `binding_mismatch_total` — open rows found newly contradicted by their
    /// pane's own record naming ANOTHER session: each transition INTO
    /// [`BindingState::Unknown`], so a mismatch that persists is counted once,
    /// and a pane's prior tenant (see [`binding_verdicts`]) is never counted.
    pub fn mismatches(&self) -> u64 {
        self.mismatches.load(Ordering::Relaxed)
    }

    /// `binding_move_refused_total` — refusal EPISODES: binds the store
    /// refused because they would have moved a row off a live pane that still
    /// runs that session, counted once per (session, refused pane) until that
    /// refusal stops recurring.
    pub fn refused_moves(&self) -> u64 {
        self.refused_moves.load(Ordering::Relaxed)
    }

    /// One row newly contradicted: the warn line and the count. `evidenced` is
    /// the session the pane's own record names.
    pub(crate) fn note_mismatch(
        &self,
        terminal_id: &str,
        bound_session: &str,
        evidenced_session: &str,
    ) {
        self.mismatches.fetch_add(1, Ordering::Relaxed);
        warn!(
            terminal_id,
            bound_session, evidenced_session, "session binder: binding mismatch"
        );
    }

    /// A new refusal episode. See [`Self::refused_moves`].
    pub(crate) fn note_move_refused(&self, session: &str, held_by: &str, refused_terminal: &str) {
        self.refused_moves.fetch_add(1, Ordering::Relaxed);
        warn!(
            claude_session = session,
            held_by,
            refused_terminal,
            "session binder: refused to move a session's row off a live pane that still runs it"
        );
    }
}

/// [`BindingCounters::mismatches`] of the global counters.
pub fn binding_mismatch_total() -> u64 {
    BindingCounters::global().mismatches()
}

/// [`BindingCounters::refused_moves`] of the global counters.
pub fn binding_move_refused_total() -> u64 {
    BindingCounters::global().refused_moves()
}

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

/// One open row's verdict against its pane's evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingVerdict {
    pub claude_session_id: String,
    pub terminal_id: String,
    pub state: BindingState,
    /// The session the pane's own record names.
    pub evidenced: String,
    /// Whether a transition into `Unknown` is a FAULT worth the warn line and
    /// the counter — `false` for a pane's prior tenant.
    pub counted: bool,
}

/// Pure: a verdict for every open row on a pane whose evidence NAMES a session
/// `E`. A row on a pane with no `claude`, unreadable evidence, or no entry in
/// `evidence` gets no verdict and keeps the state it last read. For a row `R`
/// on such a pane:
///
/// 1. `R == E` → `Matched`.
/// 2. Another live pane's record names `R` → `Unknown`, counted: the row sits
///    on one pane while its session runs in another — the 2026-10-25 shape.
/// 3. `E` also has an open row on this pane, and the provider's hook recorded
///    `E` STARTING here (`last_session_start_at`) — the pane changed sessions
///    and the store already knows its current one (a `/clear` or an in-TUI
///    resume):
///    - when the hook recorded `R` starting LATER than `E`, the record may
///      simply lag the switch the hook reported → no verdict. This is the arm
///      that keeps a newer, hook-recorded row from reading `unknown` if the
///      record does not follow `/clear` (see the module docs: that it does is
///      unmeasured);
///    - otherwise `R` is the pane's PRIOR TENANT → `Unknown`, not counted: its
///      pane runs another session now, which is routine.
/// 4. Otherwise → `Unknown`, counted: the pane runs a session its rows do not
///    name — including a pane whose current row exists only because the
///    reconcile pass just bound it from that very record (no hook recorded it
///    starting).
pub fn binding_verdicts(
    open: &[TerminalSessionRecord],
    evidence: &HashMap<String, PaneEvidence>,
) -> Vec<BindingVerdict> {
    let mut named_by: HashMap<&str, Vec<&str>> = HashMap::new();
    for (terminal, ev) in evidence {
        if let Some(id) = ev.named_session() {
            named_by.entry(id).or_default().push(terminal.as_str());
        }
    }
    let mut open_on: HashMap<&str, Vec<&TerminalSessionRecord>> = HashMap::new();
    for r in open.iter().filter(|r| r.state == "open") {
        open_on.entry(r.terminal_id.as_str()).or_default().push(r);
    }
    let mut out = Vec::new();
    for (terminal, rows) in &open_on {
        let Some(evidenced) = evidence
            .get(*terminal)
            .and_then(PaneEvidence::named_session)
        else {
            continue;
        };
        let current = rows.iter().find(|r| r.claude_session_id == evidenced);
        for row in rows {
            let id = row.claude_session_id.as_str();
            let (state, counted) = if id == evidenced {
                (BindingState::Matched, false)
            } else if named_by
                .get(id)
                .is_some_and(|ts| ts.iter().any(|t| t != terminal))
            {
                (BindingState::Unknown, true)
            } else if let Some(current_start) = current.and_then(|c| c.last_session_start_at) {
                if row
                    .last_session_start_at
                    .is_some_and(|start| start > current_start)
                {
                    continue;
                }
                (BindingState::Unknown, false)
            } else {
                (BindingState::Unknown, true)
            };
            out.push(BindingVerdict {
                claude_session_id: id.to_string(),
                terminal_id: (*terminal).to_string(),
                state,
                evidenced: evidenced.to_string(),
                counted,
            });
        }
    }
    out.sort_by(|a, b| {
        (&a.terminal_id, &a.claude_session_id).cmp(&(&b.terminal_id, &b.claude_session_id))
    });
    out
}

/// Write `verdicts` to the store; log + count every row that NEWLY reads
/// `Unknown` on a counted verdict, and return those rows. A prior tenant's
/// transition is logged at debug only. A verdict for a row that moved or
/// closed since the verdict was taken is dropped by the store.
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
        if v.state != BindingState::Unknown || previous == Some(BindingState::Unknown) {
            continue;
        }
        if v.counted {
            store.binding_counters().note_mismatch(
                &v.terminal_id,
                &v.claude_session_id,
                &v.evidenced,
            );
            newly_unknown.push(v.clone());
        } else {
            debug!(
                terminal_id = %v.terminal_id,
                prior_session = %v.claude_session_id,
                current_session = %v.evidenced,
                "session binder: a pane's prior tenant no longer runs there — its row reads unknown"
            );
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

    fn panes_naming(&self, session_id: &str) -> Vec<String> {
        let mut terminals: Vec<String> = self
            .0
            .iter()
            .filter(|(_, ev)| ev.named_session() == Some(session_id))
            .map(|(terminal, _)| terminal.clone())
            .collect();
        terminals.sort();
        terminals
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::claude_activity::RecordEvidence;
    use std::sync::Arc;

    fn live(pid: u32) -> LivePid {
        LivePid {
            pid,
            process_started_ms: None,
        }
    }

    fn names(id: &str) -> PaneEvidence {
        PaneEvidence::Names(id.to_string())
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

    fn parsed(session: Option<&str>) -> RecordReading {
        RecordReading::Parsed(RecordEvidence {
            status: "idle".to_string(),
            status_updated_at_ms: None,
            last_message_ms: None,
            session_id: session.map(str::to_string),
        })
    }

    #[test]
    fn evidence_names_a_session_only_when_every_top_level_record_agrees() {
        let readings: HashMap<u32, RecordReading> = [
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
        let sessions = sessions_from_readings(&readings);
        assert_eq!(
            sessions.keys().copied().collect::<HashSet<u32>>(),
            [1, 2, 3].into_iter().collect(),
            "only a parsed record naming a session names one"
        );
        assert_eq!(evidence_from(&[], &sessions), PaneEvidence::NoClaude);
        assert_eq!(evidence_from(&[live(1), live(2)], &sessions), names("s-1"));
        assert_eq!(
            evidence_from(&[live(1), live(3)], &sessions),
            PaneEvidence::Unknown,
            "two top-level processes naming different sessions"
        );
        // A record that cannot be read is never agreement — not even beside
        // one that names the session.
        for unreadable in [4, 5, 6, 7, 8, 99] {
            assert_eq!(
                evidence_from(&[live(1), live(unreadable)], &sessions),
                PaneEvidence::Unknown,
                "pid {unreadable}"
            );
        }
    }

    fn open_row(id: &str, terminal: &str, started: Option<i64>) -> TerminalSessionRecord {
        TerminalSessionRecord {
            terminal_id: terminal.to_string(),
            last_session_start_at: started,
            ..crate::session::session_lifecycle_store::test_open_record(id)
        }
    }

    fn by_id(verdicts: &[BindingVerdict]) -> HashMap<&str, (BindingState, bool)> {
        verdicts
            .iter()
            .map(|v| (v.claude_session_id.as_str(), (v.state, v.counted)))
            .collect()
    }

    #[test]
    fn verdicts_cover_only_panes_whose_evidence_names_a_session() {
        let rows = vec![
            open_row("s-a", "t-a", None),
            open_row("s-b", "t-b", None),
            open_row("s-c", "t-c", None),
            open_row("s-d", "t-gone", None),
        ];
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a".to_string(), names("s-a")),
            ("t-b".to_string(), PaneEvidence::Unknown),
            ("t-c".to_string(), PaneEvidence::NoClaude),
        ]
        .into_iter()
        .collect();
        let verdicts = binding_verdicts(&rows, &evidence);
        assert_eq!(
            by_id(&verdicts),
            [("s-a", (BindingState::Matched, false))]
                .into_iter()
                .collect(),
            "no verdict without a named session: {verdicts:?}"
        );
    }

    /// The four arms of [`binding_verdicts`] for a row its pane does not name.
    #[test]
    fn a_contradicted_row_counts_only_when_no_routine_succession_explains_it() {
        let rows = vec![
            // t-wrong runs s-x; its row names s-a, which pane t-other runs.
            open_row("s-a", "t-wrong", None),
            // t-clear ran s-old, then `/clear` (hook-recorded) made it s-new.
            open_row("s-old", "t-clear", Some(100)),
            open_row("s-new", "t-clear", Some(200)),
            // t-lag: the hook recorded s-next AFTER s-prev, but the pane's
            // record still names s-prev.
            open_row("s-prev", "t-lag", Some(100)),
            open_row("s-next", "t-lag", Some(200)),
            // t-stray runs s-y, which no row on it names, and nobody else runs
            // its row's session.
            open_row("s-stray", "t-stray", None),
            // t-bound: its current row was bound by the reconcile pass from the
            // record (no hook start), beside an older row.
            open_row("s-wrong", "t-bound", Some(100)),
            open_row("s-real", "t-bound", None),
        ];
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-wrong".to_string(), names("s-x")),
            ("t-other".to_string(), names("s-a")),
            ("t-clear".to_string(), names("s-new")),
            ("t-lag".to_string(), names("s-prev")),
            ("t-stray".to_string(), names("s-y")),
            ("t-bound".to_string(), names("s-real")),
        ]
        .into_iter()
        .collect();
        let verdicts = binding_verdicts(&rows, &evidence);
        let got = by_id(&verdicts);
        assert_eq!(
            got.get("s-a"),
            Some(&(BindingState::Unknown, true)),
            "its session runs in another pane"
        );
        assert_eq!(got.get("s-new"), Some(&(BindingState::Matched, false)));
        assert_eq!(
            got.get("s-old"),
            Some(&(BindingState::Unknown, false)),
            "a prior tenant is unknown, not a fault"
        );
        assert_eq!(got.get("s-prev"), Some(&(BindingState::Matched, false)));
        assert_eq!(
            got.get("s-next"),
            None,
            "a row newer than the one the record names gets no verdict"
        );
        assert_eq!(got.get("s-stray"), Some(&(BindingState::Unknown, true)));
        assert_eq!(
            got.get("s-wrong"),
            Some(&(BindingState::Unknown, true)),
            "no hook recorded the current session starting: a fault, not a succession"
        );
        assert_eq!(got.get("s-real"), Some(&(BindingState::Matched, false)));
    }

    #[test]
    fn applying_verdicts_counts_each_counted_transition_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let counters = Arc::new(BindingCounters::default());
        store.attach_binding_counters(counters.clone());
        store.record_open(open_row("s-b", "t-a", Some(100)));
        store.record_open(open_row("s-old", "t-c", Some(100)));
        store.record_open(open_row("s-new", "t-c", Some(200)));
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a".to_string(), names("s-a")),
            ("t-c".to_string(), names("s-new")),
        ]
        .into_iter()
        .collect();
        let verdicts = binding_verdicts(&store.open_records(), &evidence);

        let first = apply_binding_verdicts(&store, &verdicts);
        assert_eq!(
            first
                .iter()
                .map(|v| v.claude_session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["s-b"],
            "the counted mismatch is reported; the prior tenant is not"
        );
        assert_eq!(counters.mismatches(), 1);
        assert_eq!(
            store.get("s-old").unwrap().binding_state,
            Some(BindingState::Unknown),
            "the prior tenant still reads unknown"
        );
        let again = apply_binding_verdicts(&store, &verdicts);
        assert!(again.is_empty(), "a persisting mismatch is not re-reported");
        assert_eq!(counters.mismatches(), 1, "nor re-counted");

        // The pane comes to run the row's session: the verdict clears, and a
        // later contradiction is a new episode.
        let agree: HashMap<String, PaneEvidence> =
            [("t-a".to_string(), names("s-b"))].into_iter().collect();
        apply_binding_verdicts(&store, &binding_verdicts(&store.open_records(), &agree));
        assert_eq!(
            store.get("s-b").unwrap().binding_state,
            Some(BindingState::Matched)
        );
        apply_binding_verdicts(&store, &verdicts);
        assert_eq!(counters.mismatches(), 2);
        assert_eq!(
            BindingCounters::global().mismatches(),
            binding_mismatch_total(),
            "the global accessor reads the global counters"
        );
    }

    #[test]
    fn a_registry_answers_nothing_it_did_not_observe_and_nothing_stale() {
        let registry = LivePaneRegistry::new();
        assert_eq!(registry.pane_evidence("never-seen"), PaneEvidence::NoClaude);
        assert_eq!(registry.recorded_sessions(), None);
        registry.observe(PaneObservation {
            panes: [
                ("t-empty".to_string(), Vec::new()),
                // pid 0 is never alive: a claude that has exited since.
                ("t-exited".to_string(), vec![live(0)]),
            ]
            .into_iter()
            .collect(),
            sessions: [(0, "s-gone".to_string())].into_iter().collect(),
        });
        assert_eq!(registry.pane_evidence("t-empty"), PaneEvidence::NoClaude);
        assert_eq!(registry.pane_evidence("t-exited"), PaneEvidence::NoClaude);
        assert!(registry.panes_naming("s-gone").is_empty());

        let stale = LivePaneRegistry::with_max_age(Duration::ZERO);
        stale.observe(PaneObservation::default());
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            stale.pane_evidence("t-any"),
            PaneEvidence::Unknown,
            "an observation past its max age concludes nothing"
        );
        assert_eq!(stale.recorded_sessions(), None);
    }

    /// The production chain end to end over a real config dir: a REAL
    /// `sessions/<pid>.json` for a pid that is alive (this test process), read
    /// by the same reader the pass uses, handed to the registry, and answered
    /// from memory — then the record names another session, and a second pane
    /// observes an exited pid.
    #[test]
    fn a_live_pane_registry_answers_from_real_session_records() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join(".claude");
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        let me = std::process::id();
        let write = |session: &str| {
            std::fs::write(
                config.join(format!("sessions/{me}.json")),
                format!(r#"{{"pid":{me},"sessionId":"{session}","status":"idle"}}"#),
            )
            .unwrap();
        };
        let pass = |registry: &LivePaneRegistry| {
            let pids = vec![live(me)];
            let no_proc_start = |_: u32| -> Option<String> { None };
            let readings = claude_activity::read_records(
                std::slice::from_ref(&config),
                &pids,
                &no_proc_start,
                TranscriptTail::Skip,
            );
            registry.observe(PaneObservation {
                panes: [
                    ("t-live".to_string(), pids),
                    ("t-dead".to_string(), vec![live(0)]),
                ]
                .into_iter()
                .collect(),
                sessions: sessions_from_readings(&readings),
            });
        };

        let registry = LivePaneRegistry::new();
        write("s-first");
        pass(&registry);
        assert_eq!(registry.pane_evidence("t-live"), names("s-first"));
        assert_eq!(registry.panes_naming("s-first"), vec!["t-live".to_string()]);
        assert_eq!(
            registry.recorded_sessions().unwrap().get(&me),
            Some(&RecordedSession {
                process_started_ms: None,
                session_id: "s-first".to_string()
            })
        );
        assert_eq!(registry.pane_evidence("t-dead"), PaneEvidence::NoClaude);

        // Rewriting the file changes nothing until the next pass reads it: the
        // registry answers from memory.
        write("s-second");
        assert_eq!(registry.pane_evidence("t-live"), names("s-first"));
        pass(&registry);
        assert_eq!(registry.pane_evidence("t-live"), names("s-second"));
        assert!(registry.panes_naming("s-first").is_empty());

        // A record that names no session leaves the pane unknown.
        std::fs::write(
            config.join(format!("sessions/{me}.json")),
            format!(r#"{{"pid":{me},"status":"idle"}}"#),
        )
        .unwrap();
        pass(&registry);
        assert_eq!(registry.pane_evidence("t-live"), PaneEvidence::Unknown);
    }

    /// The pass at its population entry
    /// ([`crate::session::reconcile::run_reconcile_pass`]) over REAL files: two
    /// panes, each `claude`'s own `sessions/<pid>.json` naming its session, and
    /// pane C's confirmed row naming a session its record contradicts. The pass
    /// reads the records itself, binds every pane from its own record (rung
    /// 2b), counts the one contradiction, heals it in the same pass, and hands
    /// what it read to the store's oracle.
    #[tokio::test]
    async fn the_pass_binds_each_pane_from_its_own_record_and_publishes_what_it_read() {
        use crate::session::reconcile::{run_reconcile_pass, LivePty};
        const WORKDIR: &str = "/work/three-panes";
        let ambient = crate::test_env::isolated_ambient();
        let config = ambient.dir().join(".claude");
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        // (terminal, shell, claude, started secs, the session its record names)
        let panes = [
            (
                "term-a",
                4_300_001_u32,
                4_300_002_u32,
                1_700_000_000_i64,
                "s-a",
            ),
            ("term-b", 4_300_003, 4_300_004, 1_699_999_900, "s-b"),
            ("term-c", 4_300_005, 4_300_006, 1_699_999_800, "s-c"),
        ];
        let mut snapshot = ProcessSnapshot::default();
        let mut live = Vec::new();
        for (terminal, shell, claude, started, session) in panes {
            snapshot.parent_map.insert(shell, vec![claude]);
            snapshot.names.insert(shell, "bash".to_string());
            snapshot.names.insert(claude, "claude".to_string());
            snapshot.creation_times.insert(claude, started);
            std::fs::write(
                config.join(format!("sessions/{claude}.json")),
                format!(
                    r#"{{"pid":{claude},"sessionId":"{session}","startedAt":{},"status":"idle"}}"#,
                    started * 1000
                ),
            )
            .unwrap();
            let transcript =
                crate::terminal::transcript::session_transcript_path(&config, WORKDIR, session);
            std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
            std::fs::write(
                &transcript,
                "{\"type\":\"user\",\"timestamp\":\"2023-11-14T22:30:00Z\"}\n",
            )
            .unwrap();
            live.push(LivePty {
                terminal_id: terminal.to_string(),
                pid: Some(shell),
                working_dir: WORKDIR.to_string(),
                page_id: "default".to_string(),
                title: terminal.to_string(),
                ai_pid: None,
                ai_start_unix: None,
            });
        }
        let store = SessionLifecycleStore::open(ambient.dir().join("s.json")).unwrap();
        store.attach_pane_evidence(Arc::new(LivePaneRegistry::new()));
        let counters = Arc::new(BindingCounters::default());
        store.attach_binding_counters(counters.clone());
        let unconfirmed = |id: &str, terminal: &str| TerminalSessionRecord {
            working_dir: Some(WORKDIR.to_string()),
            origin: Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string()),
            ..open_row(id, terminal, None)
        };
        store.record_open(unconfirmed("s-a", "term-a"));
        store.record_open(unconfirmed("s-b", "term-b"));
        // An earlier binder left s-x CONFIRMED on pane C, which runs s-c.
        store.record_open(TerminalSessionRecord {
            confirmed_at: Some(1),
            ..unconfirmed("s-x", "term-c")
        });

        let actions = run_reconcile_pass(&store, &live, &snapshot, "test").await;

        for (terminal, _, _, _, session) in panes {
            let row = store
                .get(session)
                .unwrap_or_else(|| panic!("{session}: {actions:?}"));
            assert_eq!(
                (row.state.as_str(), row.terminal_id.as_str()),
                ("open", terminal),
                "{session}: {actions:?}"
            );
            assert_eq!(row.binding_state, Some(BindingState::Matched), "{session}");
            assert!(
                row.confirmed_at.is_some(),
                "{session}: its transcript is on disk"
            );
        }
        assert!(
            actions.iter().any(|a| matches!(
                a,
                crate::session::reconcile::ReconcileAction::BindingMismatch { session_id, evidenced, .. }
                    if session_id == "s-x" && evidenced == "s-c"
            )),
            "{actions:?}"
        );
        assert_eq!(counters.mismatches(), 1);
        assert_eq!(counters.refused_moves(), 0);
        assert_eq!(
            store.get("s-x").unwrap().state,
            "closed",
            "the healed pane's stale tenant is retired by the confirmed bind"
        );
        let recorded = store
            .recorded_sessions()
            .expect("the pass published what it read");
        for (_, _, claude, started, session) in panes {
            assert_eq!(
                recorded.get(&claude),
                Some(&RecordedSession {
                    process_started_ms: Some(started * 1000),
                    session_id: session.to_string()
                })
            );
        }
    }
}
