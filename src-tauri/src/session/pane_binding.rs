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
//! 139 carried a session id in their process's argv, and all 139 equalled the
//! record's `sessionId`. That is what a process that never switched shows, and
//! also what a record that lags a switch shows, so the sample does not settle
//! it.
//!
//! What follows from not knowing: a record may LAG a switch its own process
//! made. The lag rule ([`record_may_lag`]) discounts a pane's record when the
//! store itself knows of such a switch on that pane — a row there adopted from
//! the named session by a `/clear`, or one whose hook-recorded start on that
//! pane, inside its current `claude` process, is newer than the named
//! session's. A discounted record is read as [`PaneEvidence::Unknown`]: it
//! refuses only a WEAK move (as any unreadable pane does), claims nothing,
//! binds nothing and earns no `matched`; only arm 2 of [`binding_verdicts`],
//! which rests on ANOTHER pane's record, still judges the pane's rows. A
//! switch the store never heard of (a hookless pane) leaves the record taken
//! at face value — that limit is the assumption's, and it stands.
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
//!   the pass's observation — it reads no Claude Code record, only a liveness
//!   probe per pid — so the guard adds no record read to a bind on a tokio
//!   worker or the Tauri main thread. (The bind's own write-ahead-log append
//!   is the store's, unchanged.)
//! - **The tracking-health census** (`/restart-readiness`) attributes each
//!   top-level terminal-hosted `claude` pid by the verdict the pass stored for
//!   the row that owns its pane, so it applies the same rules — the lag rule
//!   included — and reads no record of its own.
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

    /// When the pane's current top-level `claude` started (the newest of them,
    /// millis), per a fresh observation — what [`record_may_lag`] measures an
    /// in-process switch against. `None` when unknown.
    fn pane_started_ms(&self, _terminal_id: &str) -> Option<i64> {
        None
    }

    /// Whether the oracle holds an observation fresh enough to stand on — what
    /// a reader of the verdicts the reconcile pass STORED asks before serving
    /// them as current (the tracking-health census).
    fn has_fresh_observation(&self) -> bool {
        false
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
/// - nothing observed yet, or a pane the latest pass did not see live →
///   [`PaneEvidence::NoClaude`] — fresh or stale, the registry knows nothing
///   that could hold a row there (a pane from before a restart, a pane closed
///   since);
/// - an observation older than its max age ([`OBSERVATION_MAX_AGE`]) →
///   [`PaneEvidence::Unknown`] for the panes it saw live, whatever it saw in
///   them: a `claude` may have started, exited or switched since;
/// - otherwise a pane seen with no `claude`, or whose `claude` has since
///   exited, → [`PaneEvidence::NoClaude`], and the pass's records for the pids
///   still alive decide the rest ([`evidence_from`]).
///
/// So slow or stalled passes refuse a WEAK bind only off a pane that was live
/// when last seen — never off a dead or never-seen terminal, which is what a
/// late restore rebind moves a row off.
///
/// A `claude` started in a pane AFTER the last pass is invisible until the next
/// one (≤ one liveness tick while the poll runs); such a pane answers
/// `NoClaude`, which never refuses a move, so the gap fails open to the
/// pre-guard behaviour. A PANE created after the last observation answers
/// `NoClaude` too — for as long as the poll stalls, since a stale observation
/// still never saw it. A pid reused within that window would keep the old
/// process's record — and every use of `Names` is conservative (it keeps a row
/// where it is, or spares it from a supersede), so the error is bounded to
/// keeping a row one tick longer.
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

    /// The latest observation and whether it is still fresh; `None` when nothing
    /// was observed yet. A poisoned lock still holds the last observation: it
    /// is read as STALE, so the panes it saw live answer `Unknown` and no
    /// other — never agreement, and never a guard switched off for every pane.
    fn latest(&self) -> Option<(PaneObservation, bool)> {
        match self.latest.lock() {
            Ok(held) => held
                .as_ref()
                .map(|(observation, at)| (observation.clone(), at.elapsed() <= self.max_age)),
            Err(poisoned) => {
                warn!(
                    "session binder: pane registry lock poisoned — its last observation is read as \
                     stale: the panes it saw live read unknown"
                );
                poisoned
                    .into_inner()
                    .as_ref()
                    .map(|(observation, _)| (observation.clone(), false))
            }
        }
    }

    /// The latest observation while it is fresh enough to stand on.
    fn fresh(&self) -> Option<PaneObservation> {
        self.latest()
            .and_then(|(observation, fresh)| fresh.then_some(observation))
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
        match self.latest() {
            None => PaneEvidence::NoClaude,
            Some((observation, true)) => Self::live_evidence(&observation, terminal_id),
            Some((observation, false)) if observation.panes.contains_key(terminal_id) => {
                PaneEvidence::Unknown
            }
            Some((_, false)) => PaneEvidence::NoClaude,
        }
    }

    fn panes_naming(&self, session_id: &str) -> Vec<String> {
        let Some(observation) = self.fresh() else {
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

    fn has_fresh_observation(&self) -> bool {
        self.fresh().is_some()
    }

    fn pane_started_ms(&self, terminal_id: &str) -> Option<i64> {
        let observation = self.fresh()?;
        observation
            .panes
            .get(terminal_id)?
            .iter()
            .filter(|p| crate::process_capture::health::pid_alive(p.pid))
            .filter_map(|p| p.process_started_ms)
            .max()
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
// The lag rule
// ---------------------------------------------------------------------------

/// May pane `terminal_id`'s record naming `named` merely LAG a switch the
/// pane's own process made — one the store learned of? True when another open
/// row ON THAT PANE shows it:
///
/// - the row was adopted from `named` by a `/clear` there (`adopted_from ==
///   named`, which the `clear` hook writes) — unless its hook-recorded start
///   predates the pane's current `claude` process, i.e. that `/clear` happened
///   in an earlier process whose record this one is not; or
/// - the row's hook-recorded start (`last_session_start_at`) is newer than
///   `named`'s own on this pane, and both — or the row's alone, when `named`
///   has none here — fall inside the pane's current `claude` process (at or
///   after `pane_started_ms`): a switch that process made, whose record may not
///   have followed (UNMEASURED — module docs).
///
/// Pane-local by construction: a row's stamp is dropped whenever the row moves
/// to another terminal (the hook route re-stamps the moves it makes), so every
/// stamp compared here was taken on this pane, and a row that arrived by a
/// rebind or a strong bind carries none. Without `pane_started_ms` no stamp can
/// be placed inside the process, and only the `/clear` arm applies.
///
/// One gap is left, knowingly: a `/clear` successor that later MOVES keeps its
/// `adopted_from` (coord's adoption chain reads it), so on its new pane the
/// `/clear` arm still discounts a record naming its predecessor. A row moved
/// there wrongly is then judged by arm 2 of [`binding_verdicts`] alone — counted
/// while another pane's record names it, not once nothing runs it.
///
/// Every reader that takes `Names(S)` as "the pane runs S" applies it: the
/// verdicts ([`binding_verdicts`], via [`discount_lagging`]), the reconcile
/// pass's claims and its provider-registry rung, and the store's binding guard,
/// supersede spare and bind-time `matched`. `open` is every row; closed rows
/// and rows on other panes are skipped.
pub fn record_may_lag<'a>(
    open: impl IntoIterator<Item = &'a TerminalSessionRecord>,
    terminal_id: &str,
    named: &str,
    pane_started_ms: Option<i64>,
) -> bool {
    let in_process = |stamp: Option<i64>| match (stamp, pane_started_ms) {
        (Some(stamp), Some(started)) if stamp >= started => Some(stamp),
        _ => None,
    };
    let predates_process = |stamp: Option<i64>| matches!((stamp, pane_started_ms), (Some(stamp), Some(started)) if stamp < started);
    let mut named_stamp: Option<i64> = None;
    let mut cleared_from_named = false;
    let mut newest_other: Option<i64> = None;
    for r in open
        .into_iter()
        .filter(|r| r.state == "open" && r.terminal_id == terminal_id)
    {
        if r.claude_session_id == named {
            named_stamp = in_process(r.last_session_start_at);
            continue;
        }
        if r.adopted_from.as_deref() == Some(named) && !predates_process(r.last_session_start_at) {
            cleared_from_named = true;
        }
        if let Some(stamp) = in_process(r.last_session_start_at) {
            newest_other = Some(newest_other.map_or(stamp, |n| n.max(stamp)));
        }
    }
    cleared_from_named || newest_other.is_some_and(|other| named_stamp.is_none_or(|n| other > n))
}

/// `evidence` with every pane whose record may lag ([`record_may_lag`])
/// answered [`PaneEvidence::Unknown`] instead of `Names`. `pane_started` gives
/// each pane's current `claude` start. One pass over `open` plus each pane's
/// own rows — no per-pane scan of every row.
pub fn discount_lagging(
    open: &[TerminalSessionRecord],
    evidence: &HashMap<String, PaneEvidence>,
    pane_started: &dyn Fn(&str) -> Option<i64>,
) -> HashMap<String, PaneEvidence> {
    let mut rows_on: HashMap<&str, Vec<&TerminalSessionRecord>> = HashMap::new();
    for r in open.iter().filter(|r| r.state == "open") {
        rows_on.entry(r.terminal_id.as_str()).or_default().push(r);
    }
    evidence
        .iter()
        .map(|(terminal, ev)| {
            let lagging = ev.named_session().is_some_and(|named| {
                rows_on.get(terminal.as_str()).is_some_and(|rows| {
                    record_may_lag(
                        rows.iter().copied(),
                        terminal,
                        named,
                        pane_started(terminal),
                    )
                })
            });
            let effective = if lagging {
                PaneEvidence::Unknown
            } else {
                ev.clone()
            };
            (terminal.clone(), effective)
        })
        .collect()
}

/// The rows reading `matched` that a pane's record names while it may lag
/// ([`record_may_lag`]), as (session, terminal): their `matched` was taken
/// before the switch and is no longer evidenced, so the pass withdraws it —
/// through [`SessionLifecycleStore::withdraw_lagging_matched`], which re-checks
/// the condition under the registry lock. A row reading `unknown` keeps its
/// verdict: withdrawing it would only let the same contradiction be counted
/// again.
pub fn matched_rows_on_lagging_panes(
    open: &[TerminalSessionRecord],
    evidence: &HashMap<String, PaneEvidence>,
    pane_started: &dyn Fn(&str) -> Option<i64>,
) -> Vec<(String, String)> {
    let effective = discount_lagging(open, evidence, pane_started);
    open.iter()
        .filter(|r| r.state == "open" && r.binding_state == Some(BindingState::Matched))
        .filter(|r| {
            evidence
                .get(&r.terminal_id)
                .and_then(PaneEvidence::named_session)
                == Some(r.claude_session_id.as_str())
                && effective
                    .get(&r.terminal_id)
                    .is_some_and(|e| e.named_session().is_none())
        })
        .map(|r| (r.claude_session_id.clone(), r.terminal_id.clone()))
        .collect()
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

/// Pure: a verdict for every open row on a pane whose own record NAMES a
/// session. A row on a pane with no `claude`, unreadable evidence, or no entry
/// in `evidence` gets no verdict, and keeps the state it last read.
///
/// The lag rule comes first ([`discount_lagging`], with each pane's current
/// `claude` start from `pane_started`): a pane whose record may lag a switch
/// its process made names nothing EFFECTIVELY. For a row `R` on a pane whose
/// record names `E`:
///
/// 1. `R == E`, the record not discounted → `Matched`.
/// 2. Another live pane's record (not discounted) names `R` → `Unknown`,
///    counted: the row sits on one pane while its session runs in another —
///    the 2026-10-25 shape. This arm holds on a DISCOUNTED pane too: it rests
///    on the other pane's record, not on this one's.
/// 3. On a discounted pane, nothing else is judged.
/// 4. `R` is `E`'s PRIOR TENANT on this pane → `Unknown`, not counted: its pane
///    runs another session now, which is routine. Prior means `E` was adopted
///    from `R` by a `/clear` (`adopted_from`), or the hook recorded both
///    starting on this pane, `R` first (stamps are pane-local — a row that
///    moved here carries none).
/// 5. Otherwise → `Unknown` — counted only for a CONFIRMED row. An unconfirmed
///    row is the spawn seam's pin, a provisional sniff or an unconfirmed bind:
///    a pane whose `claude` runs another id than its pin (a duplicate pane, a
///    shim-bypassed launch, a hook that has not posted yet, a resume) leaves
///    exactly such a row behind, routinely. A CONFIRMED row its pane's record
///    contradicts, with no succession to explain it — a restore rebind or a
///    strong bind that put it on the wrong pane — is the fault this counts.
pub fn binding_verdicts(
    open: &[TerminalSessionRecord],
    evidence: &HashMap<String, PaneEvidence>,
    pane_started: &dyn Fn(&str) -> Option<i64>,
) -> Vec<BindingVerdict> {
    let effective = discount_lagging(open, evidence, pane_started);
    let mut named_by: HashMap<&str, Vec<&str>> = HashMap::new();
    for (terminal, ev) in &effective {
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
        // What the pane's record names at all; `None` = no verdict on this pane.
        let Some(recorded) = evidence
            .get(*terminal)
            .and_then(PaneEvidence::named_session)
        else {
            continue;
        };
        // What it names once the lag rule is applied; `None` = discounted.
        let effective_named = effective
            .get(*terminal)
            .and_then(PaneEvidence::named_session);
        let current =
            effective_named.and_then(|named| rows.iter().find(|r| r.claude_session_id == named));
        for row in rows {
            let id = row.claude_session_id.as_str();
            let runs_elsewhere = named_by
                .get(id)
                .is_some_and(|ts| ts.iter().any(|t| t != terminal));
            let (state, counted) = if Some(id) == effective_named {
                (BindingState::Matched, false)
            } else if runs_elsewhere {
                (BindingState::Unknown, true)
            } else if effective_named.is_none() {
                continue;
            } else if current.is_some_and(|c| succeeded(c, row)) {
                (BindingState::Unknown, false)
            } else {
                (BindingState::Unknown, row.confirmed_at.is_some())
            };
            out.push(BindingVerdict {
                claude_session_id: id.to_string(),
                terminal_id: (*terminal).to_string(),
                state,
                evidenced: recorded.to_string(),
                counted,
            });
        }
    }
    out.sort_by(|a, b| {
        (&a.terminal_id, &a.claude_session_id).cmp(&(&b.terminal_id, &b.claude_session_id))
    });
    out
}

/// Did `current` succeed `prior` on the pane they share? By a `/clear`
/// (`current` adopted from `prior`), or by both starting there, `prior` first —
/// the stamps are pane-local, so both were taken on this pane.
fn succeeded(current: &TerminalSessionRecord, prior: &TerminalSessionRecord) -> bool {
    current.adopted_from.as_deref() == Some(prior.claude_session_id.as_str())
        || matches!(
            (prior.last_session_start_at, current.last_session_start_at),
            (Some(prior_start), Some(current_start)) if prior_start < current_start
        )
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
/// answers its evidence (and, when given, its `claude`'s start), and any other
/// terminal is not a live pane.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FixedPanes {
    pub panes: HashMap<String, PaneEvidence>,
    pub started: HashMap<String, i64>,
}

#[cfg(test)]
impl FixedPanes {
    pub(crate) fn of(panes: &[(&str, PaneEvidence)]) -> std::sync::Arc<Self> {
        Self::of_started(panes, &[])
    }

    /// [`Self::of`], each listed pane's current `claude` started at the given
    /// millis.
    pub(crate) fn of_started(
        panes: &[(&str, PaneEvidence)],
        started: &[(&str, i64)],
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            panes: panes
                .iter()
                .map(|(terminal, evidence)| (terminal.to_string(), evidence.clone()))
                .collect(),
            started: started
                .iter()
                .map(|(terminal, at)| (terminal.to_string(), *at))
                .collect(),
        })
    }
}

#[cfg(test)]
impl PaneEvidenceOracle for FixedPanes {
    fn pane_evidence(&self, terminal_id: &str) -> PaneEvidence {
        self.panes
            .get(terminal_id)
            .cloned()
            .unwrap_or(PaneEvidence::NoClaude)
    }

    fn panes_naming(&self, session_id: &str) -> Vec<String> {
        let mut terminals: Vec<String> = self
            .panes
            .iter()
            .filter(|(_, ev)| ev.named_session() == Some(session_id))
            .map(|(terminal, _)| terminal.clone())
            .collect();
        terminals.sort();
        terminals
    }

    fn pane_started_ms(&self, terminal_id: &str) -> Option<i64> {
        self.started.get(terminal_id).copied()
    }

    fn has_fresh_observation(&self) -> bool {
        true
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

    /// A CONFIRMED open row for `id` on `terminal`, whose hook recorded it
    /// starting at `started` (when given).
    fn open_row(id: &str, terminal: &str, started: Option<i64>) -> TerminalSessionRecord {
        TerminalSessionRecord {
            terminal_id: terminal.to_string(),
            last_session_start_at: started,
            confirmed_at: Some(1),
            ..crate::session::session_lifecycle_store::test_open_record(id)
        }
    }

    /// The spawn seam's pin of a pane: authoritative, never confirmed.
    fn seam_row(id: &str, terminal: &str) -> TerminalSessionRecord {
        TerminalSessionRecord {
            confirmed_at: None,
            origin: Some(crate::session::session_lifecycle_store::ORIGIN_AUTHORITATIVE.to_string()),
            ..open_row(id, terminal, None)
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
        let verdicts = binding_verdicts(&rows, &evidence, &|_| Some(0));
        assert_eq!(
            by_id(&verdicts),
            [("s-a", (BindingState::Matched, false))]
                .into_iter()
                .collect(),
            "no verdict without a named session: {verdicts:?}"
        );
    }

    /// The arms of [`binding_verdicts`] for a row its pane does not name, and the
    /// lag rule that withholds them — all but arm 2.
    #[test]
    fn a_contradicted_row_counts_only_when_no_routine_succession_explains_it() {
        let rows = vec![
            // t-wrong runs s-x; its row names s-a, which pane t-other runs.
            open_row("s-a", "t-wrong", None),
            // t-clear ran s-old, then `/clear` (hook-recorded) made it s-new.
            open_row("s-old", "t-clear", Some(100)),
            open_row("s-new", "t-clear", Some(200)),
            // t-lag: the hook recorded s-next AFTER s-prev inside the pane's
            // current process, but the pane's record still names s-prev — it
            // may lag the switch. s-moved arrived by a bind (no stamp), and
            // pane t-runs-it runs its session: arm 2 holds even here.
            open_row("s-prev", "t-lag", Some(100)),
            open_row("s-next", "t-lag", Some(200)),
            open_row("s-moved", "t-lag", None),
            // t-seam: the pane's claude runs s-z, which has no row there; the
            // spawn seam's unconfirmed pin is left behind.
            seam_row("s-pin", "t-seam"),
            // t-stray runs s-y, which no row on it names, and nobody else runs
            // its row's session.
            open_row("s-stray", "t-stray", None),
            // t-bound: its current row has no hook start; s-wrong's was taken
            // in an EARLIER process on the pane — no succession explains it.
            open_row("s-wrong", "t-bound", Some(100)),
            open_row("s-real", "t-bound", None),
        ];
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-wrong".to_string(), names("s-x")),
            ("t-other".to_string(), names("s-a")),
            ("t-clear".to_string(), names("s-new")),
            ("t-lag".to_string(), names("s-prev")),
            ("t-runs-it".to_string(), names("s-moved")),
            ("t-seam".to_string(), names("s-z")),
            ("t-stray".to_string(), names("s-y")),
            ("t-bound".to_string(), names("s-real")),
        ]
        .into_iter()
        .collect();
        // Every pane's current claude started at 0, but t-bound's at 150.
        let started = |terminal: &str| Some(if terminal == "t-bound" { 150 } else { 0 });
        let verdicts = binding_verdicts(&rows, &evidence, &started);
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
        assert_eq!(
            (got.get("s-prev"), got.get("s-next")),
            (None, None),
            "a record that may lag a switch inside its process judges neither the row it names \
             nor the newer one"
        );
        assert_eq!(
            got.get("s-moved"),
            Some(&(BindingState::Unknown, true)),
            "arm 2 rests on another pane's record, so it holds on a discounted pane too"
        );
        assert_eq!(
            got.get("s-pin"),
            Some(&(BindingState::Unknown, false)),
            "an unconfirmed pin the pane's claude has moved on from is not a fault"
        );
        assert_eq!(got.get("s-stray"), Some(&(BindingState::Unknown, true)));
        assert_eq!(
            got.get("s-wrong"),
            Some(&(BindingState::Unknown, true)),
            "a stamp from an earlier process neither lags the record nor makes a succession"
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
        // Recorded the way the hook route records it — unconfirmed — so the
        // write supersedes nothing: both rows stay open on t-c.
        store.record_open(TerminalSessionRecord {
            confirmed_at: None,
            ..open_row("s-new", "t-c", Some(200))
        });
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a".to_string(), names("s-a")),
            ("t-c".to_string(), names("s-new")),
        ]
        .into_iter()
        .collect();
        // t-a's current claude started after s-b's hook stamp: that stamp is an
        // earlier process's, and makes no record on t-a lag.
        let started = |terminal: &str| Some(if terminal == "t-a" { 150 } else { 0 });
        let verdicts = binding_verdicts(&store.open_records(), &evidence, &started);

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
        apply_binding_verdicts(
            &store,
            &binding_verdicts(&store.open_records(), &agree, &started),
        );
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
        assert_eq!(registry.pane_started_ms("never-seen"), None);
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
        stale.observe(PaneObservation {
            panes: [("t-seen".to_string(), Vec::new())].into_iter().collect(),
            sessions: HashMap::new(),
        });
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            stale.pane_evidence("t-seen"),
            PaneEvidence::Unknown,
            "a pane a stale observation saw live may run anything now"
        );
        assert_eq!(
            stale.pane_evidence("t-never-seen"),
            PaneEvidence::NoClaude,
            "a terminal no observation saw live holds nothing, fresh or stale"
        );
        assert!(stale.panes_naming("s-any").is_empty());
        assert_eq!(
            stale.pane_started_ms("t-seen"),
            None,
            "a stale observation places no switch inside a process"
        );
    }

    /// Round-3 review L5: a poisoned registry lock is read as a STALE
    /// observation, as its warn line says — the panes it saw live answer
    /// `Unknown`, every other terminal `NoClaude` — never as agreement and
    /// never as no evidence for every pane.
    #[test]
    fn a_poisoned_registry_reads_as_a_stale_observation() {
        let registry = Arc::new(LivePaneRegistry::new());
        registry.observe(PaneObservation {
            panes: [("t-seen".to_string(), vec![live(std::process::id())])]
                .into_iter()
                .collect(),
            sessions: [(std::process::id(), "s-seen".to_string())]
                .into_iter()
                .collect(),
        });
        assert_eq!(registry.pane_evidence("t-seen"), names("s-seen"));
        assert!(registry.has_fresh_observation());
        let holder = registry.clone();
        let _ = std::thread::spawn(move || {
            let _held = holder.latest.lock().unwrap();
            panic!("poison the registry lock");
        })
        .join();
        assert!(registry.latest.is_poisoned());

        assert_eq!(registry.pane_evidence("t-seen"), PaneEvidence::Unknown);
        assert_eq!(
            registry.pane_evidence("t-never-seen"),
            PaneEvidence::NoClaude
        );
        assert!(registry.panes_naming("s-seen").is_empty());
        assert_eq!(registry.pane_started_ms("t-seen"), None);
        assert!(!registry.has_fresh_observation());
    }

    /// Review M1: while passes are slow (the observation past its max age) a
    /// late restore rebind still moves a row off a DEAD terminal — one the
    /// stale observation never saw live — and is refused only off a pane it
    /// saw live.
    #[test]
    fn a_stale_registry_lets_a_rebind_off_a_dead_terminal_through() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let registry = Arc::new(LivePaneRegistry::with_max_age(Duration::ZERO));
        store.attach_pane_evidence(registry.clone());
        store.record_open(open_row("s-restored", "t-dead", None));
        store.record_open(open_row("s-live", "t-live", None));
        registry.observe(PaneObservation {
            panes: [("t-live".to_string(), vec![live(std::process::id())])]
                .into_iter()
                .collect(),
            sessions: [(std::process::id(), "s-live".to_string())]
                .into_iter()
                .collect(),
        });
        std::thread::sleep(Duration::from_millis(2));

        assert_eq!(
            store.rebind_terminal_checked("s-restored", "t-restored-tab", 1),
            crate::session::session_lifecycle_store::RecordOpenOutcome::Recorded
        );
        assert_eq!(
            store.get("s-restored").unwrap().terminal_id,
            "t-restored-tab"
        );
        assert_eq!(
            store.rebind_terminal_checked("s-live", "t-other-tab", 1),
            crate::session::session_lifecycle_store::RecordOpenOutcome::MoveRefused {
                held_by: "t-live".to_string()
            },
            "a weak bind is still refused off a pane the stale observation saw live"
        );
    }

    /// The lag rule (round-3 review H1/M1): pane-local, and placed inside the
    /// pane's current `claude` process. It discounts a record when the store
    /// knows of a switch that process made: a `/clear` (`adopted_from`) or a
    /// newer in-process hook stamp. A stamp from an earlier process, a row that
    /// moved here (it carries no stamp), or an unknown process start places
    /// nothing; the `/clear` arm needs no stamp at all.
    #[test]
    fn a_record_lags_only_a_switch_its_own_pane_and_process_made() {
        let cleared =
            |id: &str, terminal: &str, from: &str, started: Option<i64>| TerminalSessionRecord {
                adopted_from: Some(from.to_string()),
                ..open_row(id, terminal, started)
            };
        let rows = vec![
            // t-a: s started at 100, s2 at 200, both in the process (start 50).
            open_row("s", "t-a", Some(100)),
            open_row("s2", "t-a", Some(200)),
            // t-b: s-old's stamp predates the pane's process (start 150).
            open_row("s-old", "t-b", Some(100)),
            open_row("s-now", "t-b", None),
            // t-c: a `/clear` with no stamps at all (a row from before stamps).
            open_row("c", "t-c", None),
            cleared("c2", "t-c", "c", None),
            // t-d: that `/clear` happened in an earlier process (start 150).
            open_row("d", "t-d", None),
            cleared("d2", "t-d", "d", Some(100)),
            // t-e: x was moved onto the pane and carries no stamp; the pane
            // runs t, stamped in-process.
            open_row("t", "t-e", Some(100)),
            open_row("x", "t-e", None),
        ];
        let started = |terminal: &str| match terminal {
            "t-b" | "t-d" => Some(150),
            _ => Some(50),
        };
        let lags =
            |terminal: &str, named: &str| record_may_lag(&rows, terminal, named, started(terminal));
        assert!(lags("t-a", "s"), "a newer in-process switch on the pane");
        assert!(!lags("t-a", "s2"), "the newest row is current");
        assert!(
            !record_may_lag(&rows, "t-a", "s", None),
            "with no process start, no stamp is placed inside it"
        );
        assert!(
            !lags("t-b", "s-now"),
            "an earlier process's stamp places nothing"
        );
        assert!(lags("t-c", "c"), "a `/clear` successor, stamped or not");
        assert!(
            record_may_lag(&rows, "t-c", "c", None),
            "the `/clear` arm needs no process start"
        );
        assert!(!lags("t-d", "d"), "a `/clear` in an earlier process");
        assert!(!lags("t-e", "t"), "a row that moved here makes nothing lag");
        assert!(
            lags("t-a", "s-without-a-row"),
            "a named session with no in-process stamp on the pane lags any in-process switch \
             there (its row may have moved off since)"
        );

        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a".to_string(), names("s")),
            ("t-b".to_string(), names("s-now")),
        ]
        .into_iter()
        .collect();
        let effective = discount_lagging(&rows, &evidence, &started);
        assert_eq!(effective["t-a"], PaneEvidence::Unknown);
        assert_eq!(effective["t-b"], names("s-now"));
        let verdicts = binding_verdicts(&rows, &evidence, &started);
        assert!(
            verdicts.iter().all(|v| v.terminal_id != "t-a"),
            "no verdict on the lagging pane: {verdicts:?}"
        );
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
            let pids = vec![LivePid {
                pid: me,
                process_started_ms: Some(1_234),
            }];
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
            registry.pane_started_ms("t-live"),
            Some(1_234),
            "the live claude's start, as the pass's snapshot gave it"
        );
        assert_eq!(registry.pane_evidence("t-dead"), PaneEvidence::NoClaude);
        assert_eq!(registry.pane_started_ms("t-dead"), None);

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
    /// 2b), and counts the one contradiction and heals it in the same pass.
    #[tokio::test]
    async fn the_pass_binds_each_pane_from_its_own_record_and_heals_a_contradicted_one() {
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
            confirmed_at: None,
            ..seam_row(id, terminal)
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
    }
}
