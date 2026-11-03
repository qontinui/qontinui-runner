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
//! fails on a clean `fd646a5be` with only the test added:
//! `tree: root=qontinui-runner-cleanbase head=6b06e12560bac68d1a986cdd7f37142683340911
//! dirty=clean dirty_files=0 measured=2026-11-03T07:41:13Z`, where `6b06e1256`
//! is `fd646a5be` plus that test and nothing else.
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
//! # The verdict
//!
//! For an open row `R` (session `S`) on a live pane `P`, from one fresh
//! observation ([`binding_verdicts`]):
//!
//! 1. `P`'s own record names `S` → `matched`.
//! 2. Otherwise, another live pane's record names `S` — `S` demonstrably runs
//!    elsewhere → `unknown`, counted once per episode. This is the 2026-10-25
//!    shape.
//! 3. Otherwise → no verdict. A `matched` the row carried is withdrawn; an
//!    `unknown` is kept, so one episode is never counted twice.
//!
//! **Does a record follow an in-process switch** (`/clear`, an in-TUI
//! `/resume`)? In Claude Code 2.1.316 and 2.1.317, yes. The process registers
//! its `sessions/<pid>.json` together with a subscription to its own
//! session-change signal, and the subscriber rewrites the record's `sessionId`
//! on every switch. In the installed 2.1.317 bundle that is the registration's
//! `ff((H,B)=>{…xn({sessionId:H,parkedJobId:void 0,updatedAt:Date.now()},n)})`,
//! where `ff` subscribes to the emitter that `xw` — the session switch — fires;
//! the same rewrite call is in the binary of a 2.1.316 process still running on
//! nawa (both read 2026-11-03). On those builds a pane's record names the
//! session its process runs now, and the rules judge current evidence.
//!
//! The rules assume it. On a build whose records do NOT follow switches — one
//! this has not been read for — two cases go wrong, and nothing here guards
//! against them:
//!
//! - **(i)** A session entered by an in-TUI `/resume` is named by no pane, so
//!   a stale record naming it — in the pane that launched it and has since
//!   switched away — reads its row `unknown` wherever its hook placed it,
//!   counted once per episode, and rung 2b moves the row back onto the pane
//!   whose stale record names it.
//! - **(ii)** A pane that ran `/clear` keeps naming its predecessor. Once no
//!   live pane holds or runs that predecessor, rung 2b binds it back onto the
//!   pane, authoritatively: a confirmed bind's supersede closes the row of the
//!   session the pane really runs, the pane's coord session is re-keyed to the
//!   predecessor, and each later hook from the real session records it again —
//!   the binding can alternate per hook.
//!
//! Neither occurs on 2.1.316 or 2.1.317, whose records follow every switch (the
//! bundle reading above).
//!
//! # A placement is never undone by evidence older than it
//!
//! A pass's evidence is the process table it enumerated and the records it
//! read after that, so it can predate a write that placed a row since — a
//! hook, the typed-resume sniff, a restore rebind, a spawn's pin. The store
//! remembers, in memory, when each row was last placed (created, moved onto
//! another terminal, or re-opened —
//! [`SessionLifecycleStore::placed_after`]); each [`PaneObservation`] carries
//! the instant its enumeration began ([`PaneObservation::enumerated_at`]). A
//! row placed after that instant is not moved by the pass's rungs (2b, 2, 3
//! and the mtime fallback,
//! [`SessionLifecycleStore::record_bind_checked`]) and is not read `unknown`
//! by rule 2 ([`SessionLifecycleStore::set_binding_state_from`]): the next
//! pass, enumerated after the placement, judges it. `Instant` is monotonic,
//! so no clock step reorders the two; the memory starts empty after a
//! restart, which means "no recent placement".
//!
//! # Where it is read, and who reads the result
//!
//! - **The reconcile pass** (every liveness tick,
//!   [`crate::session::reconcile::run_reconcile_pass`]) reads every live pane's
//!   top-level records ONCE, on the blocking pool under [`RECORD_READ_BOUND`],
//!   and hands the result to the store's oracle as a [`PaneObservation`], which
//!   the oracle numbers (its epoch). It judges every open row on a live pane by
//!   the rules above and stores each verdict as the row's
//!   [`BindingState`](crate::session::session_lifecycle_store::BindingState),
//!   tagged with that epoch; it binds each unbound pane from the pane's own
//!   record (the provider-registry rung).
//! - **`record_open_checked` and `rebind_terminal_checked`** ask the attached
//!   [`PaneEvidenceOracle`] before a WEAK bind — one that is not authoritative:
//!   a transcript correlation, an mtime fallback, a restore rebind, a
//!   provisional typed-resume sniff — moves a row off another terminal: a live
//!   pane whose own record names that session keeps it. A strong
//!   (authoritative) bind always proceeds, and the next pass judges it.
//!   [`LivePaneRegistry`] answers from the pass's observation — it reads no
//!   Claude Code record, only a liveness probe per pid — so the guard adds no
//!   record read to a bind on a tokio worker or the Tauri main thread. (The
//!   bind's own write-ahead-log append is the store's, unchanged.)
//! - **The tracking-health census** (`/restart-readiness`) attributes each
//!   top-level terminal-hosted `claude` pid by the verdict the pass stored for
//!   the row that owns its pane, served only while it is CURRENT
//!   ([`SessionLifecycleStore::current_binding_verdicts`]: taken from an
//!   observation the pass has started judging from, while the latest one is
//!   fresh) and only on the terminal it was judged on. A verdict from an older
//!   observation reads as unchecked. It reads no record of its own; nor do
//!   `sessions[]` and `/sessions/history`, which serve the same verdicts by
//!   the same rule.
//!
//! # What is counted
//!
//! [`BindingCounters`]: a row that turns `unknown` (rule 2) — once per episode,
//! which lasts until the row is matched, moves or closes; and a refused weak
//! move — once per (session, pane) episode, not once per retry. Served as
//! `/health` `sessionTracking.bindingMismatchTotal` and
//! `bindingMoveRefusedTotal`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::{info, warn};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneObservation {
    pub panes: HashMap<String, Vec<LivePid>>,
    pub sessions: HashMap<u32, String>,
    /// When the enumeration of the process table it was built from BEGAN —
    /// before any pid in it was read, and so no later than anything it saw.
    /// The observation is aged from here ([`OBSERVATION_MAX_AGE`]), ordered
    /// against other observations by it, and a row placed after it is newer
    /// than everything it says ([`SessionLifecycleStore::placed_after`]).
    pub enumerated_at: Instant,
}

/// An observation of no pane, enumerated now — what a test hands an oracle to
/// number an epoch.
#[cfg(test)]
impl Default for PaneObservation {
    fn default() -> Self {
        Self {
            panes: HashMap::new(),
            sessions: HashMap::new(),
            enumerated_at: Instant::now(),
        }
    }
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

    /// The epoch of the latest observation while it is fresh enough to stand
    /// on; `None` when nothing was observed or it is stale. A stored verdict is
    /// current only while it carries this epoch.
    fn current_epoch(&self) -> Option<u64> {
        None
    }

    /// [`Self::pane_evidence`] and the epoch of the observation it was read
    /// from, in ONE read — so a verdict stored from the evidence is tagged with
    /// that observation, never a newer one. The epoch is `None` when the
    /// evidence came from no fresh observation.
    fn pane_evidence_at(&self, terminal_id: &str) -> (PaneEvidence, Option<u64>) {
        (self.pane_evidence(terminal_id), self.current_epoch())
    }

    /// Each reconcile pass hands over what it saw, and learns what became of
    /// it ([`ObserveOutcome`]).
    fn observe(&self, _observation: PaneObservation) -> ObserveOutcome {
        ObserveOutcome::Unnumbered
    }
}

/// What became of an observation handed to the oracle
/// ([`PaneEvidenceOracle::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveOutcome {
    /// Stored and numbered: the verdicts taken from it carry this epoch.
    Stored(u64),
    /// Not numbered: the oracle numbers nothing — the trait's default
    /// `observe`, which keeps nothing at all — or there is no oracle. The
    /// verdicts the pass takes carry no epoch and are never current.
    Unnumbered,
    /// NOT stored: the oracle holds an observation enumerated later. A pass
    /// judges and binds nothing from it — its evidence is older than what the
    /// oracle already answers with.
    Superseded,
}

impl ObserveOutcome {
    /// The epoch the verdicts taken from it carry, if it was numbered.
    pub fn epoch(self) -> Option<u64> {
        match self {
            ObserveOutcome::Stored(epoch) => Some(epoch),
            ObserveOutcome::Unnumbered | ObserveOutcome::Superseded => None,
        }
    }
}

/// How old the latest observation may be before nothing is concluded from it:
/// three liveness ticks (the poll runs every 45 s), so one slow or skipped pass
/// never blinds the guard, but a stalled poll is not answered from a picture of
/// panes that may have changed since.
pub const OBSERVATION_MAX_AGE: Duration = Duration::from_secs(135);

/// The production oracle: the latest reconcile pass's [`PaneObservation`],
/// numbered by an epoch, aged from when its enumeration began, and answered
/// from memory. Each pid is re-checked for
/// liveness on every query, so a `claude` that has exited since the pass
/// cannot hold a row there:
///
/// - nothing observed yet, or a pane the latest pass did not see live →
///   [`PaneEvidence::NoClaude`] — fresh or stale, the registry knows nothing
///   that could hold a row there (a pane from before a restart, a pane closed
///   since);
/// - an observation older than its max age ([`OBSERVATION_MAX_AGE`]) →
///   [`PaneEvidence::Unknown`] for the panes it saw live, whatever it saw in
///   them: a `claude` may have started, exited or switched since. `Unknown`
///   names nothing, so a stale observation refuses no move, spares no sibling,
///   and has no current epoch;
/// - otherwise a pane seen with no `claude`, or whose `claude` has since
///   exited, → [`PaneEvidence::NoClaude`], and the pass's records for the pids
///   still alive decide the rest ([`evidence_from`]).
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
///
/// A poisoned lock (a panic while it was held) is read as a stale observation
/// until the next pass writes a new one through it, which recovers it; the
/// warn line fires once per poisoning.
#[derive(Debug)]
pub struct LivePaneRegistry {
    latest: Mutex<Option<Observed>>,
    max_age: Duration,
    /// The epoch the next observation is given.
    next_epoch: AtomicU64,
    /// Whether the current poisoning of `latest` has been reported.
    poison_reported: AtomicBool,
}

/// One observation as the registry holds it.
#[derive(Debug)]
struct Observed {
    observation: PaneObservation,
    at: Instant,
    epoch: u64,
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
            next_epoch: AtomicU64::new(1),
            poison_reported: AtomicBool::new(false),
        }
    }

    /// The warn line for a poisoned lock — once per poisoning.
    fn report_poisoned(&self) {
        if !self.poison_reported.swap(true, Ordering::Relaxed) {
            warn!(
                "session binder: pane registry lock poisoned — its last observation is read as \
                 stale until the next pass writes a new one"
            );
        }
    }

    /// `f` over the latest observation and whether it is still fresh; `None`
    /// when nothing was observed yet. A poisoned lock still holds the last
    /// observation: it is read as STALE — never agreement, and never a guard
    /// switched off for every pane.
    fn with_latest<R>(&self, f: impl FnOnce(&Observed, bool) -> R) -> Option<R> {
        match self.latest.lock() {
            Ok(held) => held
                .as_ref()
                .map(|observed| f(observed, observed.at.elapsed() <= self.max_age)),
            Err(poisoned) => {
                self.report_poisoned();
                poisoned
                    .into_inner()
                    .as_ref()
                    .map(|observed| f(observed, false))
            }
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
        self.pane_evidence_at(terminal_id).0
    }

    fn pane_evidence_at(&self, terminal_id: &str) -> (PaneEvidence, Option<u64>) {
        self.with_latest(|observed, fresh| {
            if fresh {
                (
                    Self::live_evidence(&observed.observation, terminal_id),
                    Some(observed.epoch),
                )
            } else if observed.observation.panes.contains_key(terminal_id) {
                (PaneEvidence::Unknown, None)
            } else {
                (PaneEvidence::NoClaude, None)
            }
        })
        .unwrap_or((PaneEvidence::NoClaude, None))
    }

    fn panes_naming(&self, session_id: &str) -> Vec<String> {
        self.with_latest(|observed, fresh| {
            if !fresh {
                return Vec::new();
            }
            let mut terminals: Vec<String> = observed
                .observation
                .panes
                .keys()
                .filter(|terminal| {
                    Self::live_evidence(&observed.observation, terminal).named_session()
                        == Some(session_id)
                })
                .cloned()
                .collect();
            terminals.sort();
            terminals
        })
        .unwrap_or_default()
    }

    fn current_epoch(&self) -> Option<u64> {
        self.with_latest(|observed, fresh| fresh.then_some(observed.epoch))
            .flatten()
    }

    fn observe(&self, observation: PaneObservation) -> ObserveOutcome {
        // The new observation replaces whatever the lock held, so a poisoned
        // lock is recovered by writing through it — then cleared, and the next
        // poisoning is reported again.
        let mut held = self.latest.lock().unwrap_or_else(|poisoned| {
            self.report_poisoned();
            poisoned.into_inner()
        });
        let at = observation.enumerated_at;
        // Two passes overlapping can hand over out of order: an observation
        // enumerated before the one held is older evidence, and is not stored
        // (a poisoned lock is still recovered by writing through it).
        if !self.latest.is_poisoned() && held.as_ref().is_some_and(|newer| newer.at > at) {
            return ObserveOutcome::Superseded;
        }
        // Numbered under the lock, so epochs rise in the order observations
        // are stored; aged from when its enumeration began.
        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
        *held = Some(Observed {
            observation,
            at,
            epoch,
        });
        drop(held);
        if self.latest.is_poisoned() {
            self.latest.clear_poison();
            self.poison_reported.store(false, Ordering::Relaxed);
            info!("session binder: pane registry lock recovered by a new observation");
        }
        ObserveOutcome::Stored(epoch)
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

    /// `binding_mismatch_total` — open rows found newly `unknown` because
    /// their session runs in ANOTHER live pane (rule 2 of
    /// [`binding_verdicts`]): each transition INTO [`BindingState::Unknown`],
    /// so a mismatch that persists is counted once per episode.
    pub fn mismatches(&self) -> u64 {
        self.mismatches.load(Ordering::Relaxed)
    }

    /// `binding_move_refused_total` — refusal EPISODES: weak binds the store
    /// refused because they would have moved a row off a live pane whose own
    /// record names that session, counted once per (session, refused pane)
    /// until that refusal stops recurring.
    pub fn refused_moves(&self) -> u64 {
        self.refused_moves.load(Ordering::Relaxed)
    }

    /// One row newly `unknown`: the warn line and the count. `runs_on` is the
    /// live pane whose own record names the row's session.
    pub(crate) fn note_mismatch(&self, terminal_id: &str, bound_session: &str, runs_on: &str) {
        self.mismatches.fetch_add(1, Ordering::Relaxed);
        warn!(
            terminal_id,
            bound_session,
            runs_on,
            "session binder: binding mismatch — the row's session runs in another live pane"
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

/// One open row's verdict ([`binding_verdicts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingVerdict {
    pub claude_session_id: String,
    pub terminal_id: String,
    /// `Some(Matched)` — rule 1; `Some(Unknown)` — rule 2; `None` — rule 3, no
    /// verdict: a `matched` the row carries is withdrawn, an `unknown` kept.
    pub state: Option<BindingState>,
    /// Rule 2: a live pane whose own record names the row's session (the first
    /// in sort order when several do).
    pub runs_on: Option<String>,
}

/// Pure: the verdict of every open row on a live pane — one with an entry in
/// `evidence`, whatever that entry shows. A row on a pane the pass did not
/// observe gets none and keeps the state it last read. For a row `R` (session
/// `S`) on pane `P` (module docs, "The verdict"):
///
/// 1. `P`'s own record names `S` → `Matched`.
/// 2. Otherwise another live pane's record names `S` → `Unknown`.
/// 3. Otherwise → `None`.
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
    for terminals in named_by.values_mut() {
        terminals.sort_unstable();
    }
    let mut out: Vec<BindingVerdict> = open
        .iter()
        .filter(|r| r.state == "open")
        .filter_map(|r| {
            let own = evidence.get(&r.terminal_id)?;
            let id = r.claude_session_id.as_str();
            let (state, runs_on) = if own.named_session() == Some(id) {
                (Some(BindingState::Matched), None)
            } else if let Some(elsewhere) = named_by
                .get(id)
                .and_then(|terminals| terminals.iter().find(|t| **t != r.terminal_id))
            {
                (Some(BindingState::Unknown), Some((*elsewhere).to_string()))
            } else {
                (None, None)
            };
            Some(BindingVerdict {
                claude_session_id: id.to_string(),
                terminal_id: r.terminal_id.clone(),
                state,
                runs_on,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (&a.terminal_id, &a.claude_session_id).cmp(&(&b.terminal_id, &b.claude_session_id))
    });
    out
}

/// Store `verdicts`, taken from the observation numbered `epoch` (`None`: from
/// no numbered observation, so never served as current —
/// [`SessionLifecycleStore::current_binding_verdicts`]). Rules 1 and 2 write
/// their state ([`SessionLifecycleStore::set_binding_state`]); rule 3 withdraws
/// a `matched` and keeps an `unknown`
/// ([`SessionLifecycleStore::withdraw_matched`], re-checked under the registry
/// lock). Every row that NEWLY reads `unknown` is logged and counted once — its
/// episode lasts until it is matched, moves or closes — and returned. A verdict
/// for a row that moved or closed since the verdicts were taken is dropped by
/// the store, and so is an `unknown` for a row placed after the evidence was
/// enumerated (`enumerated_at`; [`SessionLifecycleStore::placed_after`]).
pub fn apply_binding_verdicts(
    store: &SessionLifecycleStore,
    verdicts: &[BindingVerdict],
    epoch: Option<u64>,
    enumerated_at: Option<Instant>,
) -> Vec<BindingVerdict> {
    let mut newly_unknown = Vec::new();
    for v in verdicts {
        let Some(state) = v.state else {
            store.withdraw_matched(&v.claude_session_id, &v.terminal_id);
            continue;
        };
        let Some(previous) = store.set_binding_state_from(
            &v.claude_session_id,
            &v.terminal_id,
            state,
            epoch,
            enumerated_at,
        ) else {
            continue;
        };
        if state == BindingState::Unknown && previous != Some(BindingState::Unknown) {
            store.binding_counters().note_mismatch(
                &v.terminal_id,
                &v.claude_session_id,
                v.runs_on.as_deref().unwrap_or_default(),
            );
            newly_unknown.push(v.clone());
        }
    }
    newly_unknown
}

/// A fixed oracle for tests across the session modules: each listed terminal
/// answers its evidence, any other terminal is not a live pane, and every
/// answer is from the observation numbered `epoch` (0 unless a test sets it).
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FixedPanes {
    pub panes: HashMap<String, PaneEvidence>,
    pub epoch: u64,
}

#[cfg(test)]
impl FixedPanes {
    pub(crate) fn of(panes: &[(&str, PaneEvidence)]) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            panes: panes
                .iter()
                .map(|(terminal, evidence)| (terminal.to_string(), evidence.clone()))
                .collect(),
            epoch: 0,
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

    fn current_epoch(&self) -> Option<u64> {
        Some(self.epoch)
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

    fn by_id(verdicts: &[BindingVerdict]) -> HashMap<&str, Option<BindingState>> {
        verdicts
            .iter()
            .map(|v| (v.claude_session_id.as_str(), v.state))
            .collect()
    }

    /// The three rules, and who gets a verdict at all: every open row on a pane
    /// the pass observed, whatever that pane shows; no row on a pane it did not
    /// observe.
    #[test]
    fn a_row_is_matched_by_its_own_pane_and_unknown_only_when_its_session_runs_elsewhere() {
        let rows = vec![
            // Rule 1: the pane's own record names the row.
            open_row("s-a", "t-a", None),
            // Rule 2: t-wrong's record names another session, and the row's
            // session runs in t-other — the 2026-10-25 shape.
            open_row("s-b", "t-wrong", None),
            // Rule 2 needs no record on the row's own pane: s-c runs in t-other2
            // while its row sits on a pane with no claude.
            open_row("s-c", "t-empty", None),
            // Rule 3: the pane runs another session, but s-d runs nowhere — a
            // pane's prior tenant, a missed hook, a cold restore: no verdict.
            open_row("s-d", "t-a", None),
            // Rule 3: unreadable evidence, and nothing else names s-e.
            open_row("s-e", "t-unreadable", None),
            // A pane the pass did not observe: not judged at all.
            open_row("s-f", "t-gone", None),
            // Rule 1 beats rule 2: s-g runs in t-g AND in t-dup.
            open_row("s-g", "t-g", None),
        ];
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a", names("s-a")),
            ("t-wrong", names("s-x")),
            ("t-other", names("s-b")),
            ("t-empty", PaneEvidence::NoClaude),
            ("t-other2", names("s-c")),
            ("t-unreadable", PaneEvidence::Unknown),
            ("t-g", names("s-g")),
            ("t-dup", names("s-g")),
        ]
        .into_iter()
        .map(|(t, e)| (t.to_string(), e))
        .collect();

        let verdicts = binding_verdicts(&rows, &evidence);
        let got = by_id(&verdicts);
        assert_eq!(got.get("s-a"), Some(&Some(BindingState::Matched)));
        assert_eq!(got.get("s-b"), Some(&Some(BindingState::Unknown)));
        assert_eq!(got.get("s-c"), Some(&Some(BindingState::Unknown)));
        assert_eq!(got.get("s-d"), Some(&None));
        assert_eq!(got.get("s-e"), Some(&None));
        assert_eq!(got.get("s-f"), None, "an unobserved pane is not judged");
        assert_eq!(got.get("s-g"), Some(&Some(BindingState::Matched)));
        let runs_on = |id: &str| {
            verdicts
                .iter()
                .find(|v| v.claude_session_id == id)
                .and_then(|v| v.runs_on.clone())
        };
        assert_eq!(runs_on("s-b").as_deref(), Some("t-other"));
        assert_eq!(runs_on("s-c").as_deref(), Some("t-other2"));
        assert_eq!(runs_on("s-a"), None);
    }

    /// Rule 2 survives a record that does not follow a switch. Pane A launched
    /// `s`, ran `/clear` into `s2`, and still names `s`; `s` was then resumed in
    /// pane B, which names it. `s` is matched where it runs, and nothing on A is
    /// `unknown`: a stale record naming `s` reaches only a row of `s` on a pane
    /// whose own record does not name `s`, and there is none.
    #[test]
    fn a_stale_record_naming_a_session_its_real_pane_also_names_marks_nothing_unknown() {
        let rows = vec![open_row("s2", "t-a", None), open_row("s", "t-b", None)];
        let evidence: HashMap<String, PaneEvidence> = [
            ("t-a".to_string(), names("s")),
            ("t-b".to_string(), names("s")),
        ]
        .into_iter()
        .collect();
        let verdicts = binding_verdicts(&rows, &evidence);
        let got = by_id(&verdicts);
        assert_eq!(got.get("s"), Some(&Some(BindingState::Matched)));
        assert_eq!(got.get("s2"), Some(&None), "judged on nothing, not unknown");
    }

    /// Applying verdicts: a rule-2 transition is counted once per episode and
    /// reported; a rule-3 row's `matched` is withdrawn and its `unknown` kept,
    /// so the persisting contradiction is not counted again; a row matched
    /// again ends the episode, and a later contradiction is a new one. Every
    /// verdict carries the epoch it was taken in.
    #[test]
    fn applying_verdicts_counts_each_episode_once_and_withdraws_only_matched() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
        let counters = Arc::new(BindingCounters::default());
        store.attach_binding_counters(counters.clone());
        store.record_open(open_row("s-b", "t-a", None));
        store.record_open(open_row("s-c", "t-c", None));
        let judge = |panes: &[(&str, PaneEvidence)], epoch: u64| {
            let evidence: HashMap<String, PaneEvidence> = panes
                .iter()
                .map(|(t, e)| (t.to_string(), e.clone()))
                .collect();
            apply_binding_verdicts(
                &store,
                &binding_verdicts(&store.open_records(), &evidence),
                Some(epoch),
                None,
            )
        };
        let state = |id: &str| store.get(id).unwrap().binding_state;

        // Both rows matched by their own panes.
        assert!(judge(&[("t-a", names("s-b")), ("t-c", names("s-c"))], 1).is_empty());
        assert_eq!(
            (state("s-b"), state("s-c")),
            (Some(BindingState::Matched), Some(BindingState::Matched))
        );

        // s-b now runs in t-b while its row sits on t-a: counted and reported.
        // t-c's claude has exited: its `matched` is withdrawn, nothing counted.
        let wrong = [
            ("t-a", names("s-a")),
            ("t-b", names("s-b")),
            ("t-c", PaneEvidence::NoClaude),
        ];
        let first = judge(&wrong, 2);
        assert_eq!(
            first
                .iter()
                .map(|v| (v.claude_session_id.as_str(), v.runs_on.as_deref()))
                .collect::<Vec<_>>(),
            vec![("s-b", Some("t-b"))]
        );
        assert_eq!(counters.mismatches(), 1);
        assert_eq!(
            state("s-c"),
            None,
            "a matched row judged on nothing is withdrawn"
        );

        // The same contradiction again: not re-reported, not re-counted. And
        // once nothing names s-b, rule 3 keeps its `unknown` — withdrawing it
        // would let the same episode be counted again.
        assert!(judge(&wrong, 3).is_empty());
        assert!(judge(&[("t-a", names("s-a"))], 4).is_empty());
        assert_eq!(state("s-b"), Some(BindingState::Unknown));
        assert!(judge(&wrong, 5).is_empty());
        assert_eq!(counters.mismatches(), 1, "one episode, one count");

        // Matched again ends the episode; the next contradiction is a new one.
        assert!(judge(&[("t-a", names("s-b"))], 6).is_empty());
        assert_eq!(state("s-b"), Some(BindingState::Matched));
        assert_eq!(judge(&wrong, 7).len(), 1);
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
        assert_eq!(registry.current_epoch(), None, "nothing observed yet");
        let first = registry
            .observe(PaneObservation {
                panes: [
                    ("t-empty".to_string(), Vec::new()),
                    // pid 0 is never alive: a claude that has exited since.
                    ("t-exited".to_string(), vec![live(0)]),
                ]
                .into_iter()
                .collect(),
                sessions: [(0, "s-gone".to_string())].into_iter().collect(),
                enumerated_at: Instant::now(),
            })
            .epoch();
        assert_eq!(registry.pane_evidence("t-empty"), PaneEvidence::NoClaude);
        assert_eq!(registry.pane_evidence("t-exited"), PaneEvidence::NoClaude);
        assert!(registry.panes_naming("s-gone").is_empty());
        assert!(first.is_some());
        assert_eq!(registry.current_epoch(), first);
        let second = registry.observe(PaneObservation::default()).epoch();
        assert!(
            second > first,
            "each observation is numbered after the last"
        );
        assert_eq!(registry.current_epoch(), second);

        let stale = LivePaneRegistry::with_max_age(Duration::ZERO);
        stale.observe(PaneObservation {
            panes: [("t-seen".to_string(), Vec::new())].into_iter().collect(),
            sessions: HashMap::new(),
            enumerated_at: Instant::now(),
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
            stale.current_epoch(),
            None,
            "a stale observation makes no verdict current"
        );
    }

    /// Review LOW 1: a poisoned registry lock reads as a STALE observation —
    /// the panes it saw live answer `Unknown`, every other terminal `NoClaude`,
    /// no epoch is current — until the next pass writes through it, which
    /// recovers it.
    #[test]
    fn a_poisoned_registry_reads_stale_until_the_next_observation_recovers_it() {
        let registry = Arc::new(LivePaneRegistry::new());
        let seen = || PaneObservation {
            panes: [("t-seen".to_string(), vec![live(std::process::id())])]
                .into_iter()
                .collect(),
            sessions: [(std::process::id(), "s-seen".to_string())]
                .into_iter()
                .collect(),
            enumerated_at: Instant::now(),
        };
        registry.observe(seen());
        assert_eq!(registry.pane_evidence("t-seen"), names("s-seen"));
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
        assert_eq!(registry.current_epoch(), None);
        assert!(registry.poison_reported.load(Ordering::Relaxed));

        let epoch = registry.observe(seen()).epoch();
        assert!(!registry.latest.is_poisoned(), "recovered by the write");
        assert!(
            !registry.poison_reported.load(Ordering::Relaxed),
            "the next poisoning is reported again"
        );
        assert_eq!(registry.pane_evidence("t-seen"), names("s-seen"));
        assert_eq!(registry.panes_naming("s-seen"), vec!["t-seen".to_string()]);
        assert_eq!(registry.current_epoch(), epoch);
    }

    /// Review L5 (and round 10's L3): an observation is aged from when its
    /// process table began to be ENUMERATED, not from when the pass handed it
    /// over — a pass that spent its time reading records, on the argv query
    /// and on the prescan hands over an observation already that old. And the
    /// evidence comes with the epoch of the observation it was read from, in
    /// one read (review N2).
    #[test]
    fn an_observation_is_aged_from_when_its_enumeration_began() {
        let me = std::process::id();
        let seen = |enumerated_at: Instant| PaneObservation {
            panes: [("t-seen".to_string(), vec![live(me)])]
                .into_iter()
                .collect(),
            sessions: [(me, "s-seen".to_string())].into_iter().collect(),
            enumerated_at,
        };
        let enumerated_at = Instant::now()
            .checked_sub(Duration::from_millis(100))
            .expect("the monotonic clock has run for 100 ms");

        let slow = LivePaneRegistry::with_max_age(Duration::from_millis(50));
        let _ = slow.observe(seen(enumerated_at));
        assert_eq!(
            slow.pane_evidence_at("t-seen"),
            (PaneEvidence::Unknown, None),
            "enumerated 100 ms ago, past its 50 ms max age: stale the moment it is stored"
        );
        assert_eq!(slow.current_epoch(), None);

        let registry = LivePaneRegistry::new();
        let epoch = registry.observe(seen(Instant::now())).epoch();
        assert!(epoch.is_some());
        assert_eq!(
            registry.pane_evidence_at("t-seen"),
            (names("s-seen"), epoch)
        );
        assert_eq!(registry.current_epoch(), epoch);
    }

    /// Review N2: two passes can hand over out of order. An observation
    /// enumerated BEFORE the one held is older evidence: it is not stored, and
    /// the oracle says so (`Superseded`, which ends its pass — review F3),
    /// while the held one keeps answering.
    #[test]
    fn an_observation_enumerated_before_the_held_one_does_not_replace_it() {
        let me = std::process::id();
        let naming = |session: &str, enumerated_at: Instant| PaneObservation {
            panes: [("t-seen".to_string(), vec![live(me)])]
                .into_iter()
                .collect(),
            sessions: [(me, session.to_string())].into_iter().collect(),
            enumerated_at,
        };
        let earlier = Instant::now();
        let later = earlier + Duration::from_millis(1);
        let registry = LivePaneRegistry::new();
        let held = registry.observe(naming("s-newer", later)).epoch();
        assert!(held.is_some());
        assert_eq!(
            registry.observe(naming("s-older", earlier)),
            ObserveOutcome::Superseded
        );
        assert_eq!(
            registry.pane_evidence_at("t-seen"),
            (names("s-newer"), held)
        );
    }

    /// The guard over the production registry: a WEAK bind (a restore rebind)
    /// is refused only off a live pane whose own record names the session.
    /// While passes are slow (the observation past its max age) nothing is
    /// refused — `Unknown` names nothing — so a late restore rebind goes
    /// through, off a dead terminal or a live one.
    #[test]
    fn a_weak_rebind_is_refused_only_off_a_pane_whose_fresh_record_names_the_session() {
        let observe = |registry: &LivePaneRegistry| {
            registry.observe(PaneObservation {
                panes: [("t-live".to_string(), vec![live(std::process::id())])]
                    .into_iter()
                    .collect(),
                sessions: [(std::process::id(), "s-live".to_string())]
                    .into_iter()
                    .collect(),
                enumerated_at: Instant::now(),
            });
        };
        let store_over = |registry: Arc<LivePaneRegistry>| {
            let dir = tempfile::tempdir().unwrap();
            let store = SessionLifecycleStore::open(dir.path().join("s.json")).unwrap();
            store.attach_pane_evidence(registry);
            store.record_open(open_row("s-restored", "t-dead", None));
            store.record_open(open_row("s-live", "t-live", None));
            (dir, store)
        };

        let fresh = Arc::new(LivePaneRegistry::new());
        observe(&fresh);
        let (_dir, store) = store_over(fresh);
        assert_eq!(
            store.rebind_terminal_checked("s-live", "t-other-tab", 1),
            crate::session::session_lifecycle_store::RecordOpenOutcome::MoveRefused {
                held_by: "t-live".to_string()
            }
        );
        assert_eq!(
            store.rebind_terminal_checked("s-restored", "t-restored-tab", 1),
            crate::session::session_lifecycle_store::RecordOpenOutcome::Recorded
        );

        let stale = Arc::new(LivePaneRegistry::with_max_age(Duration::ZERO));
        observe(&stale);
        std::thread::sleep(Duration::from_millis(2));
        let (_dir, store) = store_over(stale);
        assert_eq!(
            store.rebind_terminal_checked("s-live", "t-other-tab", 1),
            crate::session::session_lifecycle_store::RecordOpenOutcome::Recorded,
            "a stale observation refuses nothing"
        );
        assert_eq!(store.get("s-live").unwrap().terminal_id, "t-other-tab");
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
            registry
                .observe(PaneObservation {
                    panes: [
                        ("t-live".to_string(), pids),
                        ("t-dead".to_string(), vec![live(0)]),
                    ]
                    .into_iter()
                    .collect(),
                    sessions: sessions_from_readings(&readings),
                    enumerated_at: Instant::now(),
                })
                .epoch()
        };

        let registry = LivePaneRegistry::new();
        write("s-first");
        let first = pass(&registry);
        assert_eq!(registry.pane_evidence("t-live"), names("s-first"));
        assert_eq!(registry.panes_naming("s-first"), vec!["t-live".to_string()]);
        assert_eq!(registry.pane_evidence("t-dead"), PaneEvidence::NoClaude);
        assert_eq!(registry.current_epoch(), first);

        // Rewriting the file changes nothing until the next pass reads it: the
        // registry answers from memory.
        write("s-second");
        assert_eq!(registry.pane_evidence("t-live"), names("s-first"));
        let second = pass(&registry);
        assert_eq!(registry.pane_evidence("t-live"), names("s-second"));
        assert!(registry.panes_naming("s-first").is_empty());
        assert_eq!(registry.current_epoch(), second);

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
    /// ([`crate::session::reconcile::run_reconcile_pass`]) over REAL files: three
    /// panes, each `claude`'s own `sessions/<pid>.json` naming its session, and
    /// pane C's confirmed row naming a session nothing runs (a missed hook, or
    /// a terminal reused by a `claude` the hook never reported). The pass reads
    /// the records itself and binds every pane from its own record (rung 2b);
    /// pane C's stale row gets no verdict and no count (rule 3), and is retired
    /// by C's confirmed bind. Every verdict carries the pass's epoch.
    #[tokio::test]
    async fn the_pass_binds_each_pane_from_its_own_record_and_retires_a_stale_row() {
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
            ("term-b", 4_300_003, 4_300_005, 1_699_999_900, "s-b"),
            ("term-c", 4_300_006, 4_300_007, 1_699_999_800, "s-c"),
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

        let current = store.current_binding_verdicts();
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
            assert_eq!(
                current
                    .get(session)
                    .map(|v| (v.terminal_id.as_str(), v.state)),
                Some((terminal, BindingState::Matched)),
                "{session}: the verdict is the pass's own epoch, on its pane"
            );
            assert!(
                row.confirmed_at.is_some(),
                "{session}: its transcript is on disk"
            );
        }
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                crate::session::reconcile::ReconcileAction::BindingMismatch { .. }
            )),
            "s-x runs nowhere: no verdict, no count: {actions:?}"
        );
        assert_eq!(counters.mismatches(), 0);
        assert_eq!(counters.refused_moves(), 0);
        assert_eq!(
            store.get("s-x").unwrap().state,
            "closed",
            "the healed pane's stale tenant is retired by the confirmed bind"
        );
    }

    /// Round 10, through the pass as the runner enters it: the pass dates its
    /// evidence from when its process snapshot's enumeration BEGAN
    /// ([`ProcessSnapshot::taken_at`]). Pane A ran `s` (hook) and pane B ran
    /// `x` (hook); the snapshot is taken, then the typed-resume sniff for a
    /// line B's shell read moves `s` onto B, and only then does the pass read
    /// the records — still A naming `s`, B naming `x`. Dated from its
    /// snapshot, that evidence predates the move: `s` stays on B, unjudged.
    /// The same records dated NOW — a snapshot that carries no time — would be
    /// newer than the move, and rung 2b would take `s` back to A.
    #[tokio::test]
    async fn the_pass_dates_its_evidence_from_its_snapshot_and_leaves_a_later_placement_alone() {
        use crate::session::reconcile::{run_reconcile_pass, LivePty};
        use crate::terminal::claude_resume_sniff::{
            apply_typed_resume_effects, parse_typed_claude_resume, TypedLineTarget,
        };
        const WORKDIR: &str = "/work/placed-after";
        const S: &str = "0f9e8d7c-6b5a-4938-8271-605f4e3d2c1b";
        let ambient = crate::test_env::isolated_ambient();
        let config = ambient.dir().join(".claude");
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        // Only a dir with a `projects/` subtree is an account home whose
        // records the pass reads (`session_archive::discovery`).
        std::fs::create_dir_all(config.join("projects")).unwrap();
        // (terminal, shell, claude, started secs, the session its record names)
        let panes = [
            ("term-a", 4_300_301_u32, 4_300_302_u32, 1_700_000_000_i64, S),
            ("term-b", 4_300_303, 4_300_305, 1_699_999_900, "x"),
        ];
        let snapshot_taken = |taken_at: Option<Instant>| {
            let mut snapshot = ProcessSnapshot::default();
            for (_, shell, claude, started, _) in panes {
                snapshot.parent_map.insert(shell, vec![claude]);
                snapshot.names.insert(shell, "bash".to_string());
                snapshot.names.insert(claude, "claude".to_string());
                snapshot.creation_times.insert(claude, started);
            }
            snapshot.taken_at = taken_at;
            snapshot
        };
        let mut live = Vec::new();
        for (terminal, shell, claude, started, session) in panes {
            std::fs::write(
                config.join(format!("sessions/{claude}.json")),
                format!(
                    r#"{{"pid":{claude},"sessionId":"{session}","startedAt":{},"status":"idle"}}"#,
                    started * 1000
                ),
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
        let store_with_s_moved_onto_b = |file: &str| {
            let store = SessionLifecycleStore::open(ambient.dir().join(file)).unwrap();
            store.attach_pane_evidence(Arc::new(LivePaneRegistry::new()));
            for (id, terminal) in [(S, "term-a"), ("x", "term-b")] {
                store.record_open(TerminalSessionRecord {
                    working_dir: Some(WORKDIR.to_string()),
                    ..seam_row(id, terminal)
                });
                store.confirm_session(id);
                store.note_session_start_hook(id);
            }
            store
        };
        let sniff_moves_s_onto_b = |store: &SessionLifecycleStore| {
            let parsed = parse_typed_claude_resume(&format!("claude --resume {S}")).unwrap();
            apply_typed_resume_effects(
                Some(store),
                &parsed,
                TypedLineTarget::Shell,
                "term-b",
                WORKDIR,
                "default",
                "term-b",
                || {},
                || {},
            );
            assert_eq!(store.get(S).unwrap().terminal_id, "term-b", "precondition");
        };

        let store = store_with_s_moved_onto_b("dated.json");
        let snapshot = snapshot_taken(Some(Instant::now()));
        std::thread::sleep(Duration::from_millis(2));
        sniff_moves_s_onto_b(&store);
        let actions = run_reconcile_pass(&store, &live, &snapshot, "test").await;
        assert_eq!(
            store.get("x").unwrap().binding_state,
            Some(BindingState::Matched),
            "the records were read: B's names x ({actions:?})"
        );
        let row = store.get(S).unwrap();
        assert_eq!(
            (row.terminal_id.as_str(), row.binding_state),
            ("term-b", None),
            "evidence older than the move: {actions:?}"
        );

        let store = store_with_s_moved_onto_b("undated.json");
        sniff_moves_s_onto_b(&store);
        std::thread::sleep(Duration::from_millis(2));
        let actions = run_reconcile_pass(&store, &live, &snapshot_taken(None), "test").await;
        assert_eq!(
            store.get(S).unwrap().terminal_id,
            "term-a",
            "the same records dated after the move: {actions:?}"
        );
    }
}
