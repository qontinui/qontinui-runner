//! The unattended spawn gate — pace and hold every unattended PTY spawn
//! immediately before the PTY opens (plan
//! `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`,
//! Phase 2 / design decision D1).
//!
//! ## Why this exists
//!
//! On 2026-10-27 the operator's box took **19 gate-continuation PTY starts in
//! 17 s** and was hard-restarted. The spawn seam
//! ([`crate::resource_guard::admit_spawn`], called from the synchronous
//! `TerminalSession::spawn`) only WARNS past the warn ceiling, for unattended
//! and operator spawns alike, and it cannot wait: it is sync, and it runs AFTER
//! the coord consume-claim, so a refusal there burns the claimed work. The
//! unattended defer-at-WARN rule lived only in the continuation guard, which
//! reads the machine minutes before the PTY opens.
//!
//! This gate sits at the moment that matters — the last await before the PTY
//! create — and does two things:
//!
//! 1. **Paces.** At most one unattended PTY spawn per
//!    [`UNATTENDED_SPAWN_SPACING`] (3 s), across every unattended origin, through
//!    one process-global slot cursor and
//!    [`crate::coord_drain_state::next_release_slot`] — the drain release's
//!    pacer, not a second one. Releases are FIFO: every waiter takes a ticket,
//!    and only the lowest present ticket may release, so a newcomer — or a
//!    request door retrying at its `retry_after_ms` — never overtakes a waiter
//!    that queued before it. The thread reading behind the probe is memoized for
//!    250 ms (`health_monitor::THREAD_READING_TTL`), so a spawn paced 3 s behind
//!    its predecessor reads a number that already includes the predecessor's
//!    reader/waiter threads. That is what breaks "19 starts on one reading".
//! 2. **Holds** while [`crate::resource_guard::probe_for_spawn`] reads WARN or
//!    CRITICAL, re-probing every [`UNATTENDED_RECHECK_INTERVAL`] (5 s). The hold
//!    is bounded by [`UNATTENDED_HOLD_DEADLINE`] (10 min) measured per LOAD
//!    EPISODE, not per caller: the episode starts the moment the probe first
//!    reads WARN/CRITICAL in an unbroken run (a `Proceed` reading ends it, and
//!    so does going a whole deadline unobserved). Once the box has been loaded
//!    for the deadline, every queued spawn releases — still paced — with
//!    `why=hold_deadline`, and the seam's existing ladder applies unchanged:
//!    WARN proceeds with its log, CRITICAL refuses. Without the episode clock a
//!    serial caller would wait its own 10 minutes per spawn, in turn. Each spawn
//!    also keeps its OWN clock — when it was first held for load — and the
//!    deadline runs from the earlier of the two, so the shared episode can only
//!    shorten a wait: a brief dip to `Proceed` that restarts the episode never
//!    lengthens a hold already under way. An escalation to CRITICAL starts a
//!    separate CRITICAL clock (per episode, and per spawn), so a CRITICAL that
//!    follows a long WARN is still held for a deadline of its own before the
//!    seam's refusal can burn a claimed spawn. The episode's CRITICAL clock ends
//!    at the first non-CRITICAL reading, so a later spike starts a fresh one; a
//!    spawn held under an earlier spike keeps its own CRITICAL clock, which
//!    bounds it.
//!
//! The thresholds are the ones [`crate::resource_guard::effective_thread_ceilings`]
//! already folds (and the memory floors beside them) — the gate reads the probe's
//! verdict and never a number of its own. It ships enabled with no product
//! toggle; the two env overrides below are operator tuning knobs (`0` spacing
//! turns pacing off, `0` deadline turns holding off).
//!
//! ## Entry points, by the shape of the caller
//!
//! * [`admit`] — **task-owned** sites that may wait (a gate or work-unit
//!   continuation, which holds its coord claim; a condition check, whose run coord
//!   has already recorded). Holds and paces; coord's drain does not hold it (see
//!   "The drain" below). The release is a [`Released`], which the site refunds
//!   if its PTY create then fails.
//! * [`check_request`] — **request-scoped** doors (`POST /terminals`, the relay's
//!   remote create, the `terminal_create` proxy, the steward start path). Calls
//!   the decision ONCE and never awaits a hold: a held HTTP request is a
//!   timed-out request. A refusal is answered as the door's drain arm answers,
//!   with the code [`MACHINE_LOADED_CODE`] and a `retry_after_ms`. A release
//!   returns a [`RequestSlot`]: the door commits it immediately before the PTY
//!   create, and any refusal in between drops it, which refunds the slot. A
//!   commit waits one spacing behind the latest PTY that opened after its claim
//!   or is scheduled to open (another door's commit), counted as a waiter ahead
//!   of the whole queue meanwhile, so no two PTYs open less than a spacing apart.
//! * [`admit_or_defer`] — sites that must not hold their caller for long (the
//!   looping-agent supervisor's serial tick, coord's session-handoff `/ws` read
//!   loop, the resume door's batch). Decides once, waiting inline at most
//!   `max_wait` for a pace slot; anything longer is a DEFERRAL. A deferred work
//!   key keeps its ticket, so when its caller retries it is ahead of everything
//!   that queued after it. A door whose PTY never opens after the release (a
//!   refusal, a failed create) gives the slot back with [`Released::refund`].
//! * [`load_precheck`] — a load-only read with no slot and no ticket, for a site
//!   to skip expensive preparation (a transcript copy, a coord fetch) that the
//!   gate would only defer afterwards.
//!
//! Operator origins (`!SpawnOrigin::is_autonomous()`) pass every entry point
//! untouched; `SpawnOrigin::Unknown` is autonomous (fail closed).
//!
//! ## The drain (round-3 review decision B1)
//!
//! Coord's fleet drain is a DISPATCH FILTER: it stops new work being sent to a
//! device, and nothing that is working is touched. Every door keeps its own
//! drain arm, read before it admits the work, exactly as before this gate. The
//! gate decides only a drain that arms LATER, while the spawn waits in it, and
//! the rule is that nothing ever waits in the gate FOR the drain. A spawn parked
//! there would be counted by `/restart-readiness` ([`held_now`]) until the drain
//! lifted, so a drained runner could never reach a safe restart verdict, which
//! is the one thing a drain is for. Each site's drain read comes before its
//! gate: at its own arm, or upstream (a continuation's is the
//! `authorize_spawn` in `run_gate_continuation_inner`). `runner_spawn_sites`
//! enforces the other half — no drain read in a site's own body after its gate
//! call. Per door:
//!
//! * Work coord has already dispatched RUNS THROUGH a drain that arms while it
//!   waits in [`admit`]: a continuation holds its coord claim, and a condition
//!   check's run is already recorded `running` (coord inserts the row before it
//!   publishes the frame, and nothing ages it out), so deferring either would
//!   strand it. [`admit`] reads the drain at the release only to log it. Main
//!   spawned already-claimed continuations under a drain too.
//! * A respawn, a handoff, a resume and a looping-agent tick read the drain at
//!   their own arm, before [`admit_or_defer`], which waits at most one spacing,
//!   so nothing waits there for a drain either.
//! * The request doors, the steward start among them, read the drain before
//!   [`check_request`]: a drained device answers `device_drained`, and the gate
//!   is never consulted.
//!
//! So [`held_now`] counts only spawns that will move: held for load, or for
//! their pace slot.
//!
//! ## Log grammar
//!
//! ```text
//! resource_guard: holding unattended <what> origin=<origin> work=<key> lane=<lane> observed=<n> limit=<n> why=<warn|critical|pace>
//! resource_guard: releasing unattended <what> origin=<origin> work=<key> held_ms=<n> why=<clear|hold_deadline|pace>
//! ```
//!
//! A pace hold reports `lane=pace observed=<ms since the last unattended
//! release> limit=<spacing ms>`. A spawn that never waited logs nothing, except
//! a `hold_deadline` release, which is always logged so that population is
//! countable. A holding line is written when the hold's `why` changes, not on
//! every recheck. `refusing` (a request door) and `deferring` lines share the
//! holding line's fields. A spawn [`admit`] releases while the drain reads
//! deferring logs a `runs through coord's device drain` line.
//!
//! ## Composition
//!
//! [`decide`] is PURE so the headless launch seam (open PR #2207's
//! `admit_headless_spawn`) can compose it instead of growing a second hold state
//! machine: whichever of the two lands second collapses onto this one.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::time::Instant;
use tracing::{info, warn};

use crate::coord_drain_state::{next_release_slot, SpawnOrigin};
use crate::resource_guard::SpawnGate;

/// Minimum spacing between two unattended PTY spawns. 12x the thread reading's
/// memo TTL; 46 spawns pace over ~2.3 min instead of 17-60 s.
pub(crate) const UNATTENDED_SPAWN_SPACING: Duration = Duration::from_secs(3);

/// How often a load hold re-probes the machine.
pub(crate) const UNATTENDED_RECHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How long a load episode may hold unattended spawns before they release with
/// `why=hold_deadline`.
pub(crate) const UNATTENDED_HOLD_DEADLINE: Duration = Duration::from_secs(600);

/// The refusal code a request-scoped door answers a `Hold` verdict with.
pub(crate) const MACHINE_LOADED_CODE: &str = "machine_loaded";

/// Prefix of [`MachineLoaded::refusal_text`], for callers whose error channel is
/// a `String` and who must tell a deferral from a broken spawn
/// ([`is_machine_loaded_refusal`]).
pub(crate) const MACHINE_LOADED_PREFIX: &str = "unattended_spawn_gate:machine_loaded: ";

/// Upper bounds on the env overrides. Not policy: an `Instant + Duration` that
/// overflows PANICS, and nothing on the path to a PTY may be able to unwind.
const MAX_SPACING: Duration = Duration::from_secs(3_600);
const MAX_DEADLINE: Duration = Duration::from_secs(7 * 24 * 3_600);

/// How long one probe verdict is shared between waiters. Every waiter re-probes
/// on each wake; sharing the verdict for a second bounds the probe (and the
/// second-lane line `probe_for_spawn` logs when both lanes trip) to about one
/// per second however many spawns are held. Shorter than
/// [`UNATTENDED_SPAWN_SPACING`], so each paced spawn still reads a verdict taken
/// after its predecessor was released.
const PROBE_MEMO_TTL: Duration = Duration::from_millis(1_000);

/// Most work keys whose deferral ticket is remembered at once.
const DEFERRED_KEYS_MAX: usize = 256;

/// A remembered deferral not retried for this long (or the hold deadline, if
/// longer) is forgotten: its work most likely never came back. Two catch-up
/// ticks of the handoff loop, so a respawn retried every 60 s keeps its place.
const DEFERRED_KEY_MIN_TTL: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The gate's three durations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GateConfig {
    pub(crate) spacing: Duration,
    pub(crate) recheck: Duration,
    pub(crate) hold_deadline: Duration,
}

impl GateConfig {
    /// The shipped values.
    pub(crate) const DEFAULT: GateConfig = GateConfig {
        spacing: UNATTENDED_SPAWN_SPACING,
        recheck: UNATTENDED_RECHECK_INTERVAL,
        hold_deadline: UNATTENDED_HOLD_DEADLINE,
    };

    /// The live configuration: the defaults, with the two env overrides applied
    /// — `QONTINUI_UNATTENDED_SPAWN_SPACING_MS` (milliseconds) and
    /// `QONTINUI_UNATTENDED_HOLD_DEADLINE_SECS` (seconds). Read per decision, so
    /// a changed value applies to the next spawn.
    pub(crate) fn from_env() -> Self {
        Self::from_values(
            std::env::var("QONTINUI_UNATTENDED_SPAWN_SPACING_MS").ok(),
            std::env::var("QONTINUI_UNATTENDED_HOLD_DEADLINE_SECS").ok(),
        )
    }

    /// PURE: the configuration for raw override values. An absent, empty or
    /// non-numeric value keeps the default; a value past the overflow bound is
    /// clamped to it.
    pub(crate) fn from_values(spacing_ms: Option<String>, deadline_secs: Option<String>) -> Self {
        let parse = |raw: Option<String>| raw.and_then(|v| v.trim().parse::<u64>().ok());
        Self {
            spacing: parse(spacing_ms)
                .map_or(UNATTENDED_SPAWN_SPACING, Duration::from_millis)
                .min(MAX_SPACING),
            recheck: UNATTENDED_RECHECK_INTERVAL,
            hold_deadline: parse(deadline_secs)
                .map_or(UNATTENDED_HOLD_DEADLINE, Duration::from_secs)
                .min(MAX_DEADLINE),
        }
    }
}

// ---------------------------------------------------------------------------
// The pure decision
// ---------------------------------------------------------------------------

/// Why a spawn is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldWhy {
    /// The probe read WARN.
    Warn,
    /// The probe read CRITICAL.
    Critical,
    /// Another unattended spawn was released less than one spacing ago, or an
    /// earlier waiter is still queued.
    Pace,
}

impl HoldWhy {
    /// The `why=` token.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            HoldWhy::Warn => "warn",
            HoldWhy::Critical => "critical",
            HoldWhy::Pace => "pace",
        }
    }
}

/// Why a spawn is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReleaseWhy {
    /// It was held for load, and the load cleared.
    Clear,
    /// The load episode it met has lasted past [`GateConfig::hold_deadline`];
    /// the seam's own ladder decides from here.
    HoldDeadline,
    /// It was never held for load (or holding is switched off); at most it
    /// waited for its pace slot.
    Pace,
}

impl ReleaseWhy {
    /// The `why=` token.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReleaseWhy::Clear => "clear",
            ReleaseWhy::HoldDeadline => "hold_deadline",
            ReleaseWhy::Pace => "pace",
        }
    }
}

/// One step of the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateStep {
    /// Spawn now. The caller claims the slot (sets the cursor to `now`).
    Release(ReleaseWhy),
    /// Do not spawn yet; decide again at the instant carried.
    Hold(HoldWhy, Instant),
}

/// PURE: the gate's verdict for one spawn at `now`, first in the queue.
///
/// * `last_slot` — the instant the last unattended spawn was released (the
///   process-global cursor), `None` before the first.
/// * `verdict` — [`crate::resource_guard::probe_for_spawn`]'s reading.
/// * `held_since` — where the deadline is measured from for this verdict: the
///   earlier of this spawn's first load hold and the load episode's start (for
///   CRITICAL, the same pair on the CRITICAL clock); `None` if it has not been
///   held for load. A `Some` meeting a `Proceed` verdict releases as
///   [`ReleaseWhy::Clear`].
///
/// Load first: WARN or CRITICAL holds until a recheck, never past the deadline
/// (the recheck instant is capped at it, so a deadline release lands exactly on
/// the deadline). A zero deadline switches holding off. Past the deadline, or
/// under `Proceed`, pacing decides: the slot is [`next_release_slot`] after
/// `last_slot`, and a slot in the future is a pace hold until it.
pub(crate) fn decide(
    now: Instant,
    last_slot: Option<Instant>,
    verdict: &SpawnGate,
    held_since: Option<Instant>,
    cfg: &GateConfig,
) -> GateStep {
    decide_in_queue(now, last_slot, verdict, held_since, 0, cfg)
}

/// PURE: [`decide`] for a spawn with `ahead` earlier waiters still queued. A
/// queued spawn's earliest slot is one spacing per waiter ahead of it past the
/// next free slot, so it never releases before them (FIFO) and its hold — or a
/// request door's `retry_after_ms` — names a realistic instant.
pub(crate) fn decide_in_queue(
    now: Instant,
    last_slot: Option<Instant>,
    verdict: &SpawnGate,
    held_since: Option<Instant>,
    ahead: usize,
    cfg: &GateConfig,
) -> GateStep {
    let tripped = match verdict {
        SpawnGate::Proceed => None,
        SpawnGate::Warn(_) => Some(HoldWhy::Warn),
        SpawnGate::Critical(_) => Some(HoldWhy::Critical),
    };
    let why = match tripped {
        None if held_since.is_some() => ReleaseWhy::Clear,
        None => ReleaseWhy::Pace,
        // Holding is switched off: the spawn was not held, so it is not a
        // deadline release either.
        Some(_) if cfg.hold_deadline.is_zero() => ReleaseWhy::Pace,
        Some(load_why) => {
            let since = held_since.unwrap_or(now);
            let deadline = since.checked_add(cfg.hold_deadline);
            if deadline.is_none_or(|d| now < d) {
                let recheck = now.checked_add(cfg.recheck).unwrap_or(now);
                let at = deadline.map_or(recheck, |d| recheck.min(d));
                return GateStep::Hold(load_why, at);
            }
            ReleaseWhy::HoldDeadline
        }
    };
    let base = next_release_slot(now, last_slot, cfg.spacing, Duration::ZERO);
    let behind = cfg
        .spacing
        .saturating_mul(u32::try_from(ahead).unwrap_or(u32::MAX));
    let slot = base.checked_add(behind).unwrap_or(base);
    if slot > now {
        return GateStep::Hold(HoldWhy::Pace, slot);
    }
    GateStep::Release(why)
}

// ---------------------------------------------------------------------------
// The shared state: cursor, load episode, FIFO tickets, remembered deferrals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Episode {
    since: Instant,
    last_seen: Instant,
    /// When this episode's current run of CRITICAL readings began; `None` while
    /// it reads WARN. The first non-CRITICAL reading ends it, so a later
    /// CRITICAL (a new spike) starts a fresh clock, and a spawn arriving under it
    /// is held for a deadline of its own. A spawn held across the oscillation
    /// keeps its own CRITICAL clock ([`Holds`]), which is what bounds it.
    critical_since: Option<Instant>,
}

/// One spawn's own load clocks: when it was first held for load, and when it
/// was first held under CRITICAL. Each only ever moves EARLIER (`min` with the
/// episode's matching start), so the shared episode can shorten a wait — a
/// spawn joining a long-standing episode — but a dip that restarts the episode
/// can never lengthen one. A REMEMBERED deferral's clocks are the exception:
/// its caller has gone away, so they are cleared when the load clears (see
/// [`Inner::observe`]); otherwise a key that once passed its deadline would be
/// exempt from every later, unrelated episode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Holds {
    load: Option<Instant>,
    critical: Option<Instant>,
}

impl Holds {
    /// Fold the episode a WARN/CRITICAL reading belongs to into this spawn's
    /// clocks.
    fn fold(&mut self, episode: &Episode, verdict: &SpawnGate) {
        self.load = Some(self.load.map_or(episode.since, |h| h.min(episode.since)));
        if let (SpawnGate::Critical(_), Some(cs)) = (verdict, episode.critical_since) {
            self.critical = Some(self.critical.map_or(cs, |h| h.min(cs)));
        }
    }

    /// The clock the deadline runs on for `verdict`: the CRITICAL clock under
    /// CRITICAL, the load clock otherwise (under `Proceed`, a `Some` is what
    /// makes the release a `Clear`).
    fn clock(&self, verdict: &SpawnGate) -> Option<Instant> {
        match verdict {
            SpawnGate::Critical(_) => self.critical,
            _ => self.load,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Remembered {
    ticket: u64,
    last_seen: Instant,
    /// The deferred spawn's own clocks, carried to its retry.
    holds: Holds,
}

#[derive(Debug)]
struct Inner {
    /// The slot cursor: when the last unattended spawn was released.
    last_release: Option<Instant>,
    /// The current load episode, as last observed.
    episode: Option<Episode>,
    next_ticket: u64,
    /// Tickets of the spawns waiting right now (task waiters, and deferrable
    /// callers while they are in the call). Only these can block a release.
    present: BTreeSet<u64>,
    /// Work key -> the ticket its deferred spawn keeps for its retry.
    deferred: BTreeMap<String, Remembered>,
    /// Request doors waiting out their commit ([`RequestSlot::commit`]). Each
    /// opens a PTY when its wait ends, so it counts as one waiter ahead of every
    /// queued spawn meanwhile (round-3 review W1).
    committing: usize,
    /// When the last of those doors opens its PTY. A later commit waits one
    /// spacing behind it, as it would behind a release (round-4 review nit 1).
    committing_until: Option<Instant>,
}

impl Inner {
    const fn new() -> Self {
        Self {
            last_release: None,
            episode: None,
            next_ticket: 0,
            present: BTreeSet::new(),
            deferred: BTreeMap::new(),
            committing: 0,
            committing_until: None,
        }
    }

    /// Fold one probe reading into the load episode; the episode when the
    /// reading is WARN or CRITICAL. A `Proceed` ends the episode, and so does a
    /// gap longer than the deadline with no reading at all — an episode nobody
    /// observed for that long is not known to be unbroken. A CRITICAL reading
    /// starts the episode's CRITICAL clock unless one is running; any other
    /// reading ends it. Whenever an episode ends, every remembered deferral's
    /// clocks are cleared with it.
    fn observe(&mut self, now: Instant, verdict: &SpawnGate, cfg: &GateConfig) -> Option<Episode> {
        if verdict.tripped().is_none() {
            if self.episode.take().is_some() {
                self.clear_remembered_clocks();
            }
            return None;
        }
        let continuing = self
            .episode
            .filter(|e| now.saturating_duration_since(e.last_seen) <= cfg.hold_deadline);
        if continuing.is_none() {
            // A new episode: whatever clocks a remembered key took belong to one
            // that is over.
            self.clear_remembered_clocks();
        }
        let episode = Episode {
            since: continuing.map_or(now, |e| e.since),
            last_seen: now,
            critical_since: match verdict {
                SpawnGate::Critical(_) => {
                    Some(continuing.and_then(|e| e.critical_since).unwrap_or(now))
                }
                // Back to WARN: the spike is over (round-3 review W2).
                _ => None,
            },
        };
        self.episode = Some(episode);
        Some(episode)
    }

    /// The load cleared: no remembered deferral keeps a clock from the episode
    /// that just ended, so its retry is held afresh by the next one.
    fn clear_remembered_clocks(&mut self) {
        for r in self.deferred.values_mut() {
            r.holds = Holds::default();
        }
    }

    /// Whether a load episode is running now: one exists and was observed within
    /// the deadline, so the next reading continues it.
    fn episode_live(&self, now: Instant, cfg: &GateConfig) -> bool {
        self.episode
            .is_some_and(|e| now.saturating_duration_since(e.last_seen) <= cfg.hold_deadline)
    }

    /// Waiters queued ahead of `ticket` (every present waiter, for a caller with
    /// no ticket — a request door), plus every door waiting out its commit: its
    /// PTY opens when that wait ends, so the queue head computes one more
    /// spacing behind it and yields instead of opening at the same instant.
    fn ahead_of(&self, ticket: Option<u64>) -> usize {
        let queued = match ticket {
            Some(t) => self.present.range(..t).count(),
            None => self.present.len(),
        };
        queued + self.committing
    }

    fn allocate(&mut self) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        ticket
    }

    /// Forget remembered deferrals not retried within the TTL.
    fn evict(&mut self, now: Instant, cfg: &GateConfig) {
        let ttl = cfg.hold_deadline.max(DEFERRED_KEY_MIN_TTL);
        self.deferred
            .retain(|_, r| now.saturating_duration_since(r.last_seen) <= ttl);
    }
}

/// What one locked decision saw.
#[derive(Debug, Clone, Copy)]
struct Step {
    step: GateStep,
    now: Instant,
    last_slot: Option<Instant>,
}

/// The gate's state and counters. One process-global instance serves the
/// runner; tests build their own.
#[derive(Debug)]
pub(crate) struct GateState {
    inner: Mutex<Inner>,
    stats: GateStats,
}

impl GateState {
    pub(crate) const fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::new()),
            stats: GateStats::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue a new waiter.
    fn enqueue(&self) -> u64 {
        let mut inner = self.lock();
        let ticket = inner.allocate();
        inner.present.insert(ticket);
        ticket
    }

    /// Queue a deferrable caller under the ticket — and with the clocks — its
    /// key kept, or a new ticket and fresh clocks. Kept clocks need a live
    /// episode: with none, the next reading starts a new one, which they do not
    /// belong to.
    fn enqueue_key(&self, key: &str, cfg: &GateConfig) -> (u64, Holds) {
        let mut inner = self.lock();
        let now = Instant::now();
        inner.evict(now, cfg);
        let live = inner.episode_live(now, cfg);
        let (ticket, holds) = match inner.deferred.get(key) {
            Some(r) => (r.ticket, if live { r.holds } else { Holds::default() }),
            None => (inner.allocate(), Holds::default()),
        };
        inner.present.insert(ticket);
        (ticket, holds)
    }

    fn leave(&self, ticket: u64) {
        self.lock().present.remove(&ticket);
    }

    /// Spawns waiting inside the gate right now (see [`held_now`]).
    pub(crate) fn held(&self) -> usize {
        self.lock().present.len()
    }

    /// Keep `ticket` and `holds` for `key`'s retry.
    fn remember(&self, key: &str, ticket: u64, holds: Holds, now: Instant, cfg: &GateConfig) {
        let mut inner = self.lock();
        inner.evict(now, cfg);
        if let Some(r) = inner.deferred.get_mut(key) {
            r.last_seen = now;
            r.holds = holds;
            return;
        }
        while inner.deferred.len() >= DEFERRED_KEYS_MAX {
            let Some(oldest) = inner
                .deferred
                .iter()
                .min_by_key(|(_, r)| r.last_seen)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            inner.deferred.remove(&oldest);
        }
        inner.deferred.insert(
            key.to_string(),
            Remembered {
                ticket,
                last_seen: now,
                holds,
            },
        );
    }

    fn forget(&self, key: &str) {
        self.lock().deferred.remove(key);
    }

    /// One decision, under the lock. `now` is read here — after the caller's
    /// probe, while the cursor is held — so the cursor never records a stale
    /// instant. A WARN/CRITICAL reading folds the episode into the spawn's own
    /// clocks ([`Holds::fold`]); a release claims the slot and takes `ticket` out
    /// of the queue.
    fn step(
        &self,
        verdict: &SpawnGate,
        ticket: Option<u64>,
        holds: &mut Holds,
        cfg: &GateConfig,
    ) -> Step {
        let mut inner = self.lock();
        let now = Instant::now();
        if let Some(episode) = inner.observe(now, verdict, cfg) {
            holds.fold(&episode, verdict);
        }
        let ahead = inner.ahead_of(ticket);
        let last_slot = inner.last_release;
        let step = decide_in_queue(now, last_slot, verdict, holds.clock(verdict), ahead, cfg);
        if let GateStep::Release(_) = step {
            inner.last_release = Some(now);
            if let Some(t) = ticket {
                inner.present.remove(&t);
            }
        }
        Step {
            step,
            now,
            last_slot,
        }
    }

    /// Give back a slot a request door claimed and never used, unless another
    /// release has already moved the cursor past it.
    fn refund(&self, prev: Option<Instant>, claimed_at: Instant) {
        let mut inner = self.lock();
        if inner.last_release == Some(claimed_at) {
            inner.last_release = prev;
        }
    }

    fn health_json(&self, cfg: &GateConfig) -> serde_json::Value {
        let (held_now, deferred_now, episode) = {
            let mut inner = self.lock();
            let now = Instant::now();
            inner.evict(now, cfg);
            let episode = inner.episode.map(|e| {
                serde_json::json!({
                    "ageSecs": now.saturating_duration_since(e.since).as_secs(),
                    "criticalAgeSecs": e
                        .critical_since
                        .map(|c| now.saturating_duration_since(c).as_secs()),
                    "lastSeenSecsAgo": now.saturating_duration_since(e.last_seen).as_secs(),
                })
            });
            (inner.present.len(), inner.deferred.len(), episode)
        };
        let s = &self.stats;
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        serde_json::json!({
            "spacingMs": u64::try_from(cfg.spacing.as_millis()).unwrap_or(u64::MAX),
            "recheckMs": u64::try_from(cfg.recheck.as_millis()).unwrap_or(u64::MAX),
            "holdDeadlineSecs": cfg.hold_deadline.as_secs(),
            "heldNow": held_now,
            "deferredNow": deferred_now,
            "loadEpisode": episode,
            "releasedTotal": {
                "immediate": n(&s.released_immediate),
                "pace": n(&s.released_pace),
                "clear": n(&s.released_clear),
                "holdDeadline": n(&s.released_hold_deadline),
            },
            "refusedTotal": {
                "warn": n(&s.refused_warn),
                "critical": n(&s.refused_critical),
                "pace": n(&s.refused_pace),
            },
            "deferredTotal": {
                "warn": n(&s.deferred_warn),
                "critical": n(&s.deferred_critical),
                "pace": n(&s.deferred_pace),
            },
            "ranThroughDrainTotal": n(&s.ran_through_drain),
            "refundedTotal": n(&s.refunded),
        })
    }
}

/// The `/health` counters. Totals are since process start.
#[derive(Debug)]
pub(crate) struct GateStats {
    released_immediate: AtomicU64,
    released_pace: AtomicU64,
    released_clear: AtomicU64,
    released_hold_deadline: AtomicU64,
    refused_warn: AtomicU64,
    refused_critical: AtomicU64,
    refused_pace: AtomicU64,
    deferred_warn: AtomicU64,
    deferred_critical: AtomicU64,
    deferred_pace: AtomicU64,
    /// Spawns [`admit`] released while the drain read deferring.
    ran_through_drain: AtomicU64,
    refunded: AtomicU64,
}

impl GateStats {
    const fn new() -> Self {
        Self {
            released_immediate: AtomicU64::new(0),
            released_pace: AtomicU64::new(0),
            released_clear: AtomicU64::new(0),
            released_hold_deadline: AtomicU64::new(0),
            refused_warn: AtomicU64::new(0),
            refused_critical: AtomicU64::new(0),
            refused_pace: AtomicU64::new(0),
            deferred_warn: AtomicU64::new(0),
            deferred_critical: AtomicU64::new(0),
            deferred_pace: AtomicU64::new(0),
            ran_through_drain: AtomicU64::new(0),
            refunded: AtomicU64::new(0),
        }
    }

    fn note_release(&self, waited: bool, why: ReleaseWhy) {
        let counter = match (waited, why) {
            (false, ReleaseWhy::Pace) => &self.released_immediate,
            (_, ReleaseWhy::Pace) => &self.released_pace,
            (_, ReleaseWhy::Clear) => &self.released_clear,
            (_, ReleaseWhy::HoldDeadline) => &self.released_hold_deadline,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn note_refusal(&self, why: HoldWhy) {
        let counter = match why {
            HoldWhy::Warn => &self.refused_warn,
            HoldWhy::Critical => &self.refused_critical,
            HoldWhy::Pace => &self.refused_pace,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn note_deferral(&self, why: HoldWhy) {
        let counter = match why {
            HoldWhy::Warn => &self.deferred_warn,
            HoldWhy::Critical => &self.deferred_critical,
            HoldWhy::Pace => &self.deferred_pace,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// A queued waiter. Dropped on release AND when the waiting future is
/// cancelled, so an abandoned waiter never blocks the queue behind it.
struct Queued<'a> {
    state: &'a GateState,
    ticket: u64,
}

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        self.state.leave(self.ticket);
    }
}

static STATE: GateState = GateState::new();

/// The shared probe verdict and when it was taken (see [`PROBE_MEMO_TTL`]).
static PROBE_MEMO: Mutex<Option<(Instant, SpawnGate)>> = Mutex::new(None);

/// `/health` `spawnAdmission.unattendedGate`.
pub(crate) fn health_json() -> serde_json::Value {
    STATE.health_json(&GateConfig::from_env())
}

/// `/health`'s ONE `spawnAdmission` object with [`health_json`] folded in as
/// its `unattendedGate` field — never a second `spawnAdmission` key, which a
/// `json!` literal would let silently overwrite the first. A block that is not
/// an object (it always is) is kept, beside the gate, under `admission`.
pub(crate) fn with_unattended_gate(block: serde_json::Value) -> serde_json::Value {
    with_gate_block(block, health_json())
}

fn with_gate_block(block: serde_json::Value, gate: serde_json::Value) -> serde_json::Value {
    match block {
        serde_json::Value::Object(mut fields) => {
            fields.insert("unattendedGate".to_string(), gate);
            serde_json::Value::Object(fields)
        }
        other => serde_json::json!({ "admission": other, "unattendedGate": gate }),
    }
}

/// Unattended spawns waiting inside the gate right now, held for load or for
/// their pace slot — never for coord's drain (see the module docs). Work
/// waiting to spawn (a continuation already holds its coord claim and often a
/// worktree) that is not a session yet; `/restart-readiness` counts it as live
/// work.
pub(crate) fn held_now() -> usize {
    STATE.held()
}

/// The live probe, shared between waiters for [`PROBE_MEMO_TTL`].
fn memoized_probe() -> SpawnGate {
    let now = Instant::now();
    {
        let memo = PROBE_MEMO.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, verdict)) = memo.as_ref() {
            if now.saturating_duration_since(*at) < PROBE_MEMO_TTL {
                return verdict.clone();
            }
        }
    }
    // Probed outside the lock: it reads settings and the OS, and a waiter that
    // races this one simply probes too.
    let verdict = crate::resource_guard::probe_for_spawn();
    *PROBE_MEMO.lock().unwrap_or_else(PoisonError::into_inner) = Some((now, verdict.clone()));
    verdict
}

/// Coord's device drain for `origin`: the reason when it defers. Read by
/// [`admit`] only to log that a spawn ran through it — not a deferral, so not
/// counted on the draining banner.
fn drain_reason(origin: SpawnOrigin) -> Option<String> {
    match crate::coord_drain_state::drain_gate(origin) {
        crate::coord_drain_state::DrainGate::Allow => None,
        crate::coord_drain_state::DrainGate::Defer { reason, .. } => Some(reason),
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// `lane observed limit` for a holding line: the tripped lane's reading, or for
/// a pace hold the time since the last release against the spacing.
fn hold_fields(
    why: HoldWhy,
    verdict: &SpawnGate,
    now: Instant,
    last_slot: Option<Instant>,
    cfg: &GateConfig,
) -> (String, u64, u64) {
    match (why, verdict.tripped()) {
        (HoldWhy::Warn | HoldWhy::Critical, Some((_, obs))) => {
            (obs.lane.clone(), obs.observed, obs.limit)
        }
        _ => {
            let since = last_slot.map_or(Duration::ZERO, |l| now.saturating_duration_since(l));
            (
                "pace".to_string(),
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX),
                u64::try_from(cfg.spacing.as_millis()).unwrap_or(u64::MAX),
            )
        }
    }
}

fn log_holding(
    verb: &str,
    what: &str,
    origin: SpawnOrigin,
    work_key: &str,
    why: HoldWhy,
    fields: &(String, u64, u64),
    tail: &str,
) {
    let (lane, observed, limit) = fields;
    info!(
        "resource_guard: {verb} unattended {what} origin={origin} work={work_key} lane={lane} \
         observed={observed} limit={limit} why={}{tail}",
        why.as_str()
    );
}

fn log_releasing(what: &str, origin: SpawnOrigin, work_key: &str, held: Duration, why: ReleaseWhy) {
    let held_ms = held.as_millis();
    let deadline = why == ReleaseWhy::HoldDeadline;
    let why = why.as_str();
    if deadline {
        warn!(
            "resource_guard: releasing unattended {what} origin={origin} work={work_key} \
             held_ms={held_ms} why={why} — the spawn seam's own ladder decides from here"
        );
    } else {
        info!(
            "resource_guard: releasing unattended {what} origin={origin} work={work_key} \
             held_ms={held_ms} why={why}"
        );
    }
}

// ---------------------------------------------------------------------------
// Task-owned sites: hold and pace
// ---------------------------------------------------------------------------

/// What [`admit_with`] did. Test-facing; [`admit`] keeps only the release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// An operator origin: the gate does not apply.
    Operator,
    /// Released on the first decision, without waiting.
    Immediate,
    /// Released after waiting `held` in total.
    AfterHold { why: ReleaseWhy, held: Duration },
}

/// Hold and pace one unattended PTY spawn of work coord has already dispatched
/// (a continuation, which holds its coord claim; a condition check, whose run
/// coord has recorded). Await it IMMEDIATELY before the PTY create, after every
/// other await on the path, so the verdict it releases on is the one the PTY
/// opens under.
///
/// Returns at once for an operator origin. Otherwise it never fails: it returns
/// when the load is clear (or the episode is past the deadline) and this spawn
/// holds the next pace slot. Coord's device drain does not hold it — such work
/// runs through the drain — and a drain reading at its release is logged. The
/// seam's ladder still runs at the PTY create; a site whose create then fails
/// (that ladder's CRITICAL refusal after a `hold_deadline` release, or any
/// other failure that opened no PTY) calls [`Released::refund`].
pub(crate) async fn admit(origin: SpawnOrigin, what: &str, work_key: &str) -> Released<'static> {
    admit_released(
        origin,
        what,
        work_key,
        &GateConfig::from_env(),
        memoized_probe,
        || drain_reason(origin),
        &STATE,
    )
    .await
    .1
}

/// [`admit`] over an injected probe, drain read and state, reporting what it
/// did.
pub(crate) async fn admit_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    cfg: &GateConfig,
    probe: impl FnMut() -> SpawnGate,
    drain: impl FnMut() -> Option<String>,
    state: &GateState,
) -> Admitted {
    admit_released(origin, what, work_key, cfg, probe, drain, state)
        .await
        .0
}

/// [`admit_with`], also handing back the release.
async fn admit_released<'a>(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    cfg: &GateConfig,
    mut probe: impl FnMut() -> SpawnGate,
    mut drain: impl FnMut() -> Option<String>,
    state: &'a GateState,
) -> (Admitted, Released<'a>) {
    if !origin.is_autonomous() {
        return (Admitted::Operator, Released::unclaimed());
    }
    let started = Instant::now();
    let queued = Queued {
        state,
        ticket: state.enqueue(),
    };
    let mut holds = Holds::default();
    let mut logged: Option<HoldWhy> = None;
    let mut waited = false;
    loop {
        let verdict = probe();
        let out = state.step(&verdict, Some(queued.ticket), &mut holds, cfg);
        match out.step {
            GateStep::Release(why) => {
                state.stats.note_release(waited, why);
                let held = out.now.saturating_duration_since(started);
                if waited || why == ReleaseWhy::HoldDeadline {
                    log_releasing(what, origin, work_key, held, why);
                }
                // Dispatched work runs through the drain: read only to say so.
                if let Some(reason) = drain() {
                    state
                        .stats
                        .ran_through_drain
                        .fetch_add(1, Ordering::Relaxed);
                    info!(
                        "resource_guard: unattended {what} origin={origin} work={work_key} \
                         runs through coord's device drain — coord already dispatched it, and \
                         the drain filters dispatch, not dispatched work: {reason}"
                    );
                }
                let admitted = if waited {
                    Admitted::AfterHold { why, held }
                } else {
                    Admitted::Immediate
                };
                let released = Released {
                    claim: Some(ReleasedClaim {
                        state,
                        prev: out.last_slot,
                        at: out.now,
                    }),
                };
                return (admitted, released);
            }
            GateStep::Hold(why, at) => {
                if logged != Some(why) {
                    let fields = hold_fields(why, &verdict, out.now, out.last_slot, cfg);
                    log_holding("holding", what, origin, work_key, why, &fields, "");
                    logged = Some(why);
                }
                waited = true;
                tokio::time::sleep_until(at).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Answering without holding
// ---------------------------------------------------------------------------

/// A `Hold` verdict answered without waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineLoaded {
    pub(crate) why: HoldWhy,
    /// The spawn's queued pace slot, or the re-probe instant for a load hold.
    pub(crate) retry_after_ms: u64,
    /// Operator-readable: what was measured and when to retry.
    pub(crate) reason: String,
}

impl MachineLoaded {
    fn new(
        why: HoldWhy,
        at: Instant,
        now: Instant,
        verdict: &SpawnGate,
        what: &str,
        cfg: &GateConfig,
    ) -> Self {
        let retry_after_ms =
            u64::try_from(at.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX);
        let cause = match (why, verdict.tripped()) {
            (HoldWhy::Warn | HoldWhy::Critical, Some((severity, obs))) => obs.clause(severity),
            _ => format!(
                "autonomous spawns are paced one per {} ms and others are queued ahead",
                cfg.spacing.as_millis()
            ),
        };
        Self {
            why,
            retry_after_ms,
            reason: format!(
                "machine loaded: {cause} — not starting this {what} now; retry in \
                 {retry_after_ms} ms"
            ),
        }
    }

    /// The refusal code, [`MACHINE_LOADED_CODE`].
    pub(crate) fn code(&self) -> &'static str {
        MACHINE_LOADED_CODE
    }

    /// The HTTP 409 body: the drain refusal's shape (`api_error` + `code`), with
    /// `retry_after_ms` and the hold's `why` in `hint`.
    pub(crate) fn api_refusal(&self) -> crate::mcp::types::ApiResponse<()> {
        let mut body = crate::mcp::types::api_error(self.reason.clone());
        body.code = Some(self.code().to_string());
        body.hint = Some(serde_json::json!({
            "retry_after_ms": self.retry_after_ms,
            "why": self.why.as_str(),
        }));
        body
    }

    /// The refusal as a `String` error, prefixed with [`MACHINE_LOADED_PREFIX`].
    pub(crate) fn refusal_text(&self) -> String {
        format!("{MACHINE_LOADED_PREFIX}{}", self.reason)
    }
}

/// Whether a `String` error is a [`MachineLoaded::refusal_text`] — a deferral,
/// not a failed spawn.
pub(crate) fn is_machine_loaded_refusal(text: &str) -> bool {
    text.starts_with(MACHINE_LOADED_PREFIX)
}

/// A pace slot a request door was granted. Commit it immediately before the PTY
/// create; dropping it uncommitted — any refusal between the gate and the PTY —
/// refunds the slot, so a door that ends up spawning nothing does not delay the
/// next unattended spawn. Committing paces the PTY that actually opens: a door
/// can spend seconds between its claim and its PTY (a worktree add), so a
/// commit that finds another spawn released after the claim waits out the rest
/// of one spacing behind it, and either way moves the cursor to the PTY's real
/// opening.
#[must_use = "hold the slot until immediately before the PTY create and commit() it there; \
              dropping it refunds the slot"]
pub(crate) struct RequestSlot<'a> {
    claim: Option<Claim<'a>>,
}

struct Claim<'a> {
    state: &'a GateState,
    prev: Option<Instant>,
    at: Instant,
    why: ReleaseWhy,
    spacing: Duration,
}

impl RequestSlot<'_> {
    fn unclaimed() -> Self {
        Self { claim: None }
    }

    /// The PTY is about to open: keep the slot. If another unattended PTY
    /// opened after this door's claim — a release — or is scheduled to open — a
    /// door already waiting out its own commit — first wait one spacing behind
    /// the latest of them: one sleep, one spacing past a PTY that is at most
    /// one spacing per earlier committing door away. Meanwhile it counts as a
    /// waiter ahead of every queued spawn, so the queue head yields a spacing to
    /// this PTY instead of opening beside it. Then move the cursor to now, never
    /// backwards, so the next spawn is paced from the PTY that actually opened.
    pub(crate) async fn commit(mut self) {
        let Some(claim) = self.claim.take() else {
            return;
        };
        let committing = {
            let mut inner = claim.state.lock();
            let until = inner
                .last_release
                .max(inner.committing_until)
                .filter(|l| *l > claim.at)
                .and_then(|l| l.checked_add(claim.spacing));
            until.map(|until| {
                inner.committing += 1;
                inner.committing_until =
                    Some(inner.committing_until.map_or(until, |c| c.max(until)));
                (
                    Committing {
                        state: claim.state,
                        armed: true,
                    },
                    until,
                )
            })
        };
        let committing = match committing {
            Some((guard, until)) => {
                tokio::time::sleep_until(until).await;
                Some(guard)
            }
            None => None,
        };
        {
            let mut inner = claim.state.lock();
            let now = Instant::now();
            inner.last_release = Some(inner.last_release.map_or(now, |l| l.max(now)));
            // Un-counted under the same lock that moves the cursor, so no waiter
            // can see neither.
            if let Some(guard) = committing {
                guard.settle(&mut inner);
            }
        }
        claim.state.stats.note_release(false, claim.why);
    }
}

/// One door counted in [`Inner::committing`] for the length of its commit wait.
/// Dropped without [`Committing::settle`] — the commit future was cancelled — it
/// un-counts itself, so an abandoned commit never delays the queue forever.
struct Committing<'a> {
    state: &'a GateState,
    armed: bool,
}

impl Committing<'_> {
    fn settle(mut self, inner: &mut Inner) {
        Self::leave(inner);
        self.armed = false;
    }

    /// One door fewer. The last one out clears `committing_until`; while others
    /// remain it is the latest wake, which belongs to a door still waiting.
    fn leave(inner: &mut Inner) {
        inner.committing = inner.committing.saturating_sub(1);
        if inner.committing == 0 {
            inner.committing_until = None;
        }
    }
}

impl Drop for Committing<'_> {
    fn drop(&mut self) {
        if self.armed {
            Self::leave(&mut self.state.lock());
        }
    }
}

impl Drop for RequestSlot<'_> {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            claim.state.refund(claim.prev, claim.at);
            claim.state.stats.refunded.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The request-scoped door's gate: decided ONCE, never a wait. `Err` is answered
/// as the door's drain arm answers, with [`MACHINE_LOADED_CODE`]; `Ok` is a
/// [`RequestSlot`] to commit right before the PTY create.
///
/// The door takes no ticket, so it never jumps the queue: it is admitted only
/// when nothing is queued and the slot is free. Its load hold uses the same
/// episode deadline as every waiter, so a standing WARN refuses it only until
/// the episode is past the deadline. Call it where the door's drain arm sits —
/// before any worktree allocation or grant consumption a refusal would
/// otherwise leak.
pub(crate) fn check_request(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
) -> Result<RequestSlot<'static>, MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(RequestSlot::unclaimed());
    }
    check_request_with(
        origin,
        what,
        work_key,
        &memoized_probe(),
        &GateConfig::from_env(),
        &STATE,
    )
}

/// [`check_request`] over an injected verdict and state.
pub(crate) fn check_request_with<'a>(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    verdict: &SpawnGate,
    cfg: &GateConfig,
    state: &'a GateState,
) -> Result<RequestSlot<'a>, MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(RequestSlot::unclaimed());
    }
    let mut holds = Holds::default();
    let out = state.step(verdict, None, &mut holds, cfg);
    match out.step {
        GateStep::Release(why) => {
            if why == ReleaseWhy::HoldDeadline {
                log_releasing(what, origin, work_key, Duration::ZERO, why);
            }
            Ok(RequestSlot {
                claim: Some(Claim {
                    state,
                    prev: out.last_slot,
                    at: out.now,
                    why,
                    spacing: cfg.spacing,
                }),
            })
        }
        GateStep::Hold(why, at) => {
            state.stats.note_refusal(why);
            let loaded = MachineLoaded::new(why, at, out.now, verdict, what, cfg);
            let fields = hold_fields(why, verdict, out.now, out.last_slot, cfg);
            let tail = format!(" retry_after_ms={}", loaded.retry_after_ms);
            log_holding("refusing", what, origin, work_key, why, &fields, &tail);
            Err(loaded)
        }
    }
}

/// The gate for a site that must not hold its caller for long: decide, waiting
/// inline at most `max_wait` for a pace slot; anything longer — a load hold, or
/// a queue more than `max_wait` deep — is a DEFERRAL, answered as the site's
/// drain arm answers. The work key keeps its ticket, so its caller's retry is
/// ahead of every spawn that queued after it; a release forgets it.
///
/// `max_wait` is the caller's budget — one spacing for every caller today: the
/// looping-agent supervisor tick (3 s is far below its 120 s slot-lease TTL),
/// the `/ws` loop and the resume batch. Bounded, and it is what lets a kept
/// ticket actually win its turn against waiters that are releasing, and lets
/// several pending respawns drain at one per spacing rather than one per
/// catch-up tick. The key also keeps its own load clocks while the load episode
/// lasts, so a deferral never restarts its deadline within one episode.
///
/// It never reads coord's drain: each caller's own drain arm runs before it,
/// and it waits at most `max_wait`, so nothing waits in it for the drain (see
/// the module docs' "The drain"). A release is a [`Released`]: kept when it is
/// dropped, given back with [`Released::refund`] by a door that refuses before
/// its PTY.
pub(crate) async fn admit_or_defer(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    max_wait: Duration,
) -> Result<Released<'static>, MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(Released::unclaimed());
    }
    admit_or_defer_with(
        origin,
        what,
        work_key,
        max_wait,
        &GateConfig::from_env(),
        memoized_probe,
        &STATE,
    )
    .await
}

/// What [`admit`] and [`admit_or_defer`] hand back on a release. Its slot is
/// KEPT when it is dropped — the release sits immediately before the PTY create
/// — and a site whose PTY never opens after it (a spent account-hop budget, or a
/// create that fails, the seam's CRITICAL refusal included) gives it back with
/// [`Released::refund`], so the next unattended spawn is not paced behind a PTY
/// that never opened.
#[derive(Debug)]
pub(crate) struct Released<'a> {
    claim: Option<ReleasedClaim<'a>>,
}

#[derive(Debug)]
struct ReleasedClaim<'a> {
    state: &'a GateState,
    prev: Option<Instant>,
    at: Instant,
}

impl Released<'_> {
    fn unclaimed() -> Self {
        Self { claim: None }
    }

    /// No PTY opened: give the slot back, unless another release has already
    /// moved the cursor past it.
    pub(crate) fn refund(self) {
        if let Some(claim) = self.claim {
            claim.state.refund(claim.prev, claim.at);
            claim.state.stats.refunded.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// [`admit_or_defer`] over an injected probe and state.
pub(crate) async fn admit_or_defer_with<'a>(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    max_wait: Duration,
    cfg: &GateConfig,
    mut probe: impl FnMut() -> SpawnGate,
    state: &'a GateState,
) -> Result<Released<'a>, MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(Released::unclaimed());
    }
    let started = Instant::now();
    let (ticket, mut holds) = state.enqueue_key(work_key, cfg);
    let queued = Queued { state, ticket };
    let mut logged: Option<HoldWhy> = None;
    let mut waited = false;
    loop {
        let verdict = probe();
        let out = state.step(&verdict, Some(queued.ticket), &mut holds, cfg);
        match out.step {
            GateStep::Release(why) => {
                state.forget(work_key);
                state.stats.note_release(waited, why);
                if waited || why == ReleaseWhy::HoldDeadline {
                    log_releasing(
                        what,
                        origin,
                        work_key,
                        out.now.saturating_duration_since(started),
                        why,
                    );
                }
                return Ok(Released {
                    claim: Some(ReleasedClaim {
                        state,
                        prev: out.last_slot,
                        at: out.now,
                    }),
                });
            }
            GateStep::Hold(why, at) => {
                if why == HoldWhy::Pace && at.saturating_duration_since(started) <= max_wait {
                    if logged != Some(why) {
                        let fields = hold_fields(why, &verdict, out.now, out.last_slot, cfg);
                        log_holding("holding", what, origin, work_key, why, &fields, "");
                        logged = Some(why);
                    }
                    waited = true;
                    tokio::time::sleep_until(at).await;
                    continue;
                }
                state.remember(work_key, queued.ticket, holds, out.now, cfg);
                state.stats.note_deferral(why);
                let loaded = MachineLoaded::new(why, at, out.now, &verdict, what, cfg);
                let fields = hold_fields(why, &verdict, out.now, out.last_slot, cfg);
                let tail = format!(
                    " retry_after_ms={} (deferred; the caller retries)",
                    loaded.retry_after_ms
                );
                log_holding("deferring", what, origin, work_key, why, &fields, &tail);
                return Err(loaded);
            }
        }
    }
}

/// A load-only read with no slot claim and no ticket: `Err` when the machine
/// reads WARN or CRITICAL inside the episode deadline. For a site to skip
/// expensive preparation the gate would only defer afterwards; the gate itself
/// still runs immediately before the PTY create.
pub(crate) fn load_precheck(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
) -> Result<(), MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    load_precheck_with(
        origin,
        what,
        work_key,
        &memoized_probe(),
        &GateConfig::from_env(),
        &STATE,
    )
}

/// [`load_precheck`] over an injected verdict and state.
pub(crate) fn load_precheck_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    verdict: &SpawnGate,
    cfg: &GateConfig,
    state: &GateState,
) -> Result<(), MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    let (step, now) = {
        let mut inner = state.lock();
        let now = Instant::now();
        let mut holds = Holds::default();
        if let Some(episode) = inner.observe(now, verdict, cfg) {
            holds.fold(&episode, verdict);
        }
        (
            decide_in_queue(now, None, verdict, holds.clock(verdict), 0, cfg),
            now,
        )
    };
    match step {
        GateStep::Hold(why, at) if why != HoldWhy::Pace => {
            state.stats.note_deferral(why);
            let loaded = MachineLoaded::new(why, at, now, verdict, what, cfg);
            let fields = hold_fields(why, verdict, now, None, cfg);
            let tail = format!(
                " retry_after_ms={} (pre-check; nothing prepared)",
                loaded.retry_after_ms
            );
            log_holding("deferring", what, origin, work_key, why, &fields, &tail);
            Err(loaded)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_guard::{GateObservation, LaneMetric, CRITICAL_REFUSAL_PREFIX};
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    const S: Duration = Duration::from_secs(1);

    fn threads(observed: u64, limit: u64) -> GateObservation {
        GateObservation {
            lane: "threads".to_string(),
            metric: LaneMetric::ThreadCount,
            observed,
            limit,
        }
    }

    fn warn() -> SpawnGate {
        SpawnGate::Warn(threads(262, 256))
    }

    fn critical() -> SpawnGate {
        SpawnGate::Critical(threads(401, 400))
    }

    fn cfg() -> GateConfig {
        GateConfig::DEFAULT
    }

    fn no_drain() -> Option<String> {
        None
    }

    fn last_release(state: &GateState) -> Option<Instant> {
        state.lock().last_release
    }

    fn present(state: &GateState) -> usize {
        state.lock().present.len()
    }

    fn n(c: &AtomicU64) -> u64 {
        c.load(Ordering::SeqCst)
    }

    #[test]
    fn the_shipped_values_are_three_seconds_five_seconds_and_ten_minutes() {
        assert_eq!(UNATTENDED_SPAWN_SPACING, Duration::from_millis(3_000));
        assert_eq!(UNATTENDED_RECHECK_INTERVAL, Duration::from_millis(5_000));
        assert_eq!(UNATTENDED_HOLD_DEADLINE, Duration::from_secs(600));
        assert_eq!(GateConfig::from_values(None, None), GateConfig::DEFAULT);
    }

    #[test]
    fn the_env_overrides_parse_and_garbage_keeps_the_default() {
        let c = GateConfig::from_values(Some("1500".into()), Some("90".into()));
        assert_eq!(c.spacing, Duration::from_millis(1_500));
        assert_eq!(c.hold_deadline, Duration::from_secs(90));
        assert_eq!(c.recheck, Duration::from_secs(5));
        let g = GateConfig::from_values(Some("fast".into()), Some("".into()));
        assert_eq!(g, GateConfig::DEFAULT);
        // Zero switches each mechanism off rather than being refused.
        let z = GateConfig::from_values(Some("0".into()), Some("0".into()));
        assert_eq!(z.spacing, Duration::ZERO);
        assert_eq!(z.hold_deadline, Duration::ZERO);
        // An overflowing value is clamped, never a panic later.
        let huge = GateConfig::from_values(Some(u64::MAX.to_string()), Some(u64::MAX.to_string()));
        assert_eq!(huge.spacing, Duration::from_secs(3_600));
        assert_eq!(huge.hold_deadline, Duration::from_secs(604_800));
    }

    // --- (b) WARN holds, then Proceed releases why=clear ------------------

    #[test]
    fn warn_holds_until_a_recheck_and_proceed_then_releases_clear() {
        let t0 = Instant::now();
        assert_eq!(
            decide(t0, None, &warn(), None, &cfg()),
            GateStep::Hold(HoldWhy::Warn, t0 + Duration::from_secs(5))
        );
        assert_eq!(
            decide(t0 + 5 * S, None, &warn(), Some(t0), &cfg()),
            GateStep::Hold(HoldWhy::Warn, t0 + Duration::from_secs(10))
        );
        assert_eq!(
            decide(t0 + 10 * S, None, &SpawnGate::Proceed, Some(t0), &cfg()),
            GateStep::Release(ReleaseWhy::Clear)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_admit_under_warn_waits_and_releases_clear_when_it_clears() {
        let calls = AtomicU32::new(0);
        let probe = || {
            if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                warn()
            } else {
                SpawnGate::Proceed
            }
        };
        let state = GateState::new();
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:1",
            &cfg(),
            probe,
            no_drain,
            &state,
        )
        .await;
        assert_eq!(
            out,
            Admitted::AfterHold {
                why: ReleaseWhy::Clear,
                held: Duration::from_secs(10)
            }
        );
        assert_eq!(n(&state.stats.released_clear), 1);
        assert_eq!(present(&state), 0, "a released waiter leaves the queue");
        assert!(
            state.lock().episode.is_none(),
            "the Proceed reading ended the episode"
        );
    }

    // --- (c) the deadline is per load episode -----------------------------

    #[test]
    fn a_standing_warn_releases_at_the_deadline_never_before_it() {
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_secs(600);
        // One millisecond short of the deadline: still held, and the recheck is
        // capped AT the deadline rather than a full interval past it.
        let just_before = deadline - Duration::from_millis(1);
        assert_eq!(
            decide(just_before, None, &warn(), Some(t0), &cfg()),
            GateStep::Hold(HoldWhy::Warn, deadline)
        );
        assert_eq!(
            decide(deadline, None, &warn(), Some(t0), &cfg()),
            GateStep::Release(ReleaseWhy::HoldDeadline)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_admit_under_a_standing_warn_releases_within_one_recheck_of_the_deadline() {
        let state = GateState::new();
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:2",
            &cfg(),
            warn,
            no_drain,
            &state,
        )
        .await;
        let Admitted::AfterHold { why, held } = out else {
            panic!("a standing WARN must hold, got {out:?}");
        };
        assert_eq!(why, ReleaseWhy::HoldDeadline);
        assert!(held >= Duration::from_secs(600), "released early: {held:?}");
        assert!(
            held <= Duration::from_secs(605),
            "released later than deadline + one recheck: {held:?}"
        );
        assert_eq!(n(&state.stats.released_hold_deadline), 1);
    }

    /// W3: the deadline runs from the start of the LOAD EPISODE, not from each
    /// caller's arrival. A spawn joining a WARN that has already stood for five
    /// minutes waits about five more, not ten; one arriving after the episode
    /// passed the deadline is released at once (paced).
    #[tokio::test(start_paused = true)]
    async fn a_spawn_joining_a_standing_episode_waits_only_for_the_episode_deadline() {
        let state = Arc::new(GateState::new());
        let t0 = Instant::now();
        let first = admit_under_warn_at(state.clone(), t0, "claim:pr:a");
        let second =
            admit_under_warn_at(state.clone(), t0 + Duration::from_secs(300), "claim:pr:b");
        let (first_out, first_at) = first.await.expect("first");
        let (second_out, second_at) = second.await.expect("second");
        assert!(matches!(
            first_out,
            Admitted::AfterHold {
                why: ReleaseWhy::HoldDeadline,
                ..
            }
        ));
        assert_eq!(first_at, t0 + Duration::from_secs(600));
        let Admitted::AfterHold { why, held } = second_out else {
            panic!("the second spawn must hold, got {second_out:?}");
        };
        assert_eq!(why, ReleaseWhy::HoldDeadline);
        // Released at the episode deadline plus one pace slot behind the first.
        assert_eq!(second_at, t0 + Duration::from_secs(603));
        assert_eq!(held, Duration::from_secs(303), "not its own ten minutes");

        // A spawn arriving after the episode passed the deadline goes at once.
        tokio::time::sleep_until(t0 + Duration::from_secs(700)).await;
        let late = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:c",
            &GateConfig::DEFAULT,
            warn,
            no_drain,
            &state,
        )
        .await;
        assert_eq!(late, Admitted::Immediate);
        assert_eq!(n(&state.stats.released_hold_deadline), 3);
    }

    /// One continuation admitted under a standing WARN, arriving at `at`.
    fn admit_under_warn_at(
        state: Arc<GateState>,
        at: Instant,
        key: &'static str,
    ) -> tokio::task::JoinHandle<(Admitted, Instant)> {
        tokio::spawn(async move {
            tokio::time::sleep_until(at).await;
            let out = admit_with(
                SpawnOrigin::GateContinuation,
                "gate-continuation terminal",
                key,
                &GateConfig::DEFAULT,
                warn,
                no_drain,
                &state,
            )
            .await;
            (out, Instant::now())
        })
    }

    #[test]
    fn an_episode_unobserved_for_a_whole_deadline_starts_again() {
        let state = GateState::new();
        let t0 = Instant::now();
        let mut inner = state.lock();
        let since = |e: Option<Episode>| e.map(|e| e.since);
        assert_eq!(since(inner.observe(t0, &warn(), &cfg())), Some(t0));
        assert_eq!(
            since(inner.observe(t0 + 600 * S, &warn(), &cfg())),
            Some(t0)
        );
        // Unobserved for longer than the deadline: not known to be unbroken.
        let later = t0 + 1_201 * S;
        assert_eq!(since(inner.observe(later, &warn(), &cfg())), Some(later));
        // A Proceed reading ends it.
        assert!(inner
            .observe(later + S, &SpawnGate::Proceed, &cfg())
            .is_none());
        assert!(inner.episode.is_none());
    }

    /// Round-3 N4: a brief dip to `Proceed` — read here by a request door while
    /// the spawn is mid-hold — restarts the shared episode, but the spawn's own
    /// clock keeps the deadline where it was: released at its first hold plus
    /// the deadline, not ten minutes after the dip.
    #[tokio::test(start_paused = true)]
    async fn a_dip_to_proceed_never_lengthens_a_hold_already_under_way() {
        let state = Arc::new(GateState::new());
        let t0 = Instant::now();
        let waiter = admit_under_warn_at(state.clone(), t0, "claim:pr:dip");
        // Between two of the waiter's 5 s rechecks, so the dip is the last reading.
        tokio::time::sleep_until(t0 + Duration::from_secs(302)).await;
        // The dip: one Proceed reading, by a door the queued waiter refuses.
        let door = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        );
        assert!(door.is_err(), "the queued waiter goes first");
        assert!(state.lock().episode.is_none(), "the dip ended the episode");
        let (out, at) = waiter.await.expect("waiter");
        assert!(
            matches!(
                out,
                Admitted::AfterHold {
                    why: ReleaseWhy::HoldDeadline,
                    ..
                }
            ),
            "{out:?}"
        );
        assert!(
            at <= t0 + Duration::from_secs(605),
            "the dip lengthened the hold: released {:?} after its first hold",
            at.saturating_duration_since(t0)
        );
        assert!(at >= t0 + Duration::from_secs(600));
    }

    /// Round-3 N5: a CRITICAL that follows a long WARN starts its own clock. A
    /// spawn arriving under CRITICAL after the WARN episode has passed its
    /// deadline is still held for a full deadline before the seam's refusal
    /// can burn it.
    #[tokio::test(start_paused = true)]
    async fn critical_after_a_long_warn_is_held_on_its_own_clock() {
        let state = GateState::new();
        let t0 = Instant::now();
        // A WARN episode, observed at t0 and kept alive (re-read inside every
        // deadline-long gap) past its deadline.
        assert!(
            check_request_with(SpawnOrigin::Unknown, "x", "k", &warn(), &cfg(), &state).is_err()
        );
        tokio::time::advance(Duration::from_secs(590)).await;
        assert!(
            check_request_with(SpawnOrigin::Unknown, "x", "k", &warn(), &cfg(), &state).is_err()
        );
        tokio::time::advance(Duration::from_secs(60)).await;
        let past = check_request_with(SpawnOrigin::Unknown, "x", "k", &warn(), &cfg(), &state);
        assert!(past.is_ok(), "the WARN episode is past its deadline");
        drop(past);
        tokio::time::advance(Duration::from_secs(50)).await;
        let arrived = Instant::now();
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:crit",
            &cfg(),
            critical,
            no_drain,
            &state,
        )
        .await;
        let Admitted::AfterHold { why, held } = out else {
            panic!("CRITICAL after a long WARN must hold, got {out:?}");
        };
        assert_eq!(why, ReleaseWhy::HoldDeadline);
        assert!(held >= Duration::from_secs(600), "held only {held:?}");
        assert!(held <= Duration::from_secs(605), "held {held:?}");
        let critical_since = state.lock().episode.and_then(|e| e.critical_since);
        assert_eq!(
            critical_since,
            Some(arrived),
            "the CRITICAL clock started on escalation"
        );
        assert_eq!(
            state.lock().episode.map(|e| e.since),
            Some(t0),
            "the load episode itself is unbroken"
        );
    }

    /// Round-3 review W2: the episode's CRITICAL clock ends with its spike. A
    /// WARN episode, a brief CRITICAL spike, back to WARN for 20 min, then
    /// CRITICAL again: a spawn joining the second CRITICAL is held on a fresh
    /// clock, not released at once on the first spike's long-passed deadline.
    #[tokio::test(start_paused = true)]
    async fn a_spike_that_fell_back_to_warn_leaves_no_critical_clock_behind() {
        let state = GateState::new();
        let t0 = Instant::now();
        let read = |verdict: SpawnGate| {
            drop(check_request_with(
                SpawnOrigin::Unknown,
                "x",
                "k",
                &verdict,
                &cfg(),
                &state,
            ));
        };
        read(warn());
        tokio::time::advance(Duration::from_secs(60)).await;
        read(critical());
        tokio::time::advance(Duration::from_secs(5)).await;
        read(warn());
        assert_eq!(
            state.lock().episode.and_then(|e| e.critical_since),
            None,
            "back to WARN ends the CRITICAL clock"
        );
        // 20 minutes of WARN, re-read inside every deadline-long gap.
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(300)).await;
            read(warn());
        }
        let arrived = Instant::now();
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:second-spike",
            &cfg(),
            critical,
            no_drain,
            &state,
        )
        .await;
        let Admitted::AfterHold { why, held } = out else {
            panic!("a CRITICAL after the spike fell back to WARN must hold on a fresh clock, got {out:?}");
        };
        assert_eq!(why, ReleaseWhy::HoldDeadline);
        assert!(held >= Duration::from_secs(600), "held only {held:?}");
        assert!(held <= Duration::from_secs(605), "held {held:?}");
        assert_eq!(
            state.lock().episode.and_then(|e| e.critical_since),
            Some(arrived)
        );
        assert_eq!(
            state.lock().episode.map(|e| e.since),
            Some(t0),
            "the load episode itself is unbroken"
        );
    }

    // --- (d) CRITICAL holds the same way; the seam still refuses ----------

    #[test]
    fn critical_holds_like_warn_and_after_the_deadline_the_seam_still_refuses() {
        let t0 = Instant::now();
        assert_eq!(
            decide(t0, None, &critical(), None, &cfg()),
            GateStep::Hold(HoldWhy::Critical, t0 + Duration::from_secs(5))
        );
        let deadline = t0 + Duration::from_secs(600);
        assert_eq!(
            decide(deadline, None, &critical(), Some(t0), &cfg()),
            GateStep::Release(ReleaseWhy::HoldDeadline)
        );
        // The gate released; the seam's ladder is unchanged. The SAME verdict at
        // the PTY create is still refused (unattended callers never override)…
        let refused = crate::resource_guard::admit_verdict(
            critical(),
            "gate-continuation terminal",
            false,
            None,
        );
        let msg = refused.expect_err("CRITICAL must still be refused at the seam");
        assert!(msg.starts_with(CRITICAL_REFUSAL_PREFIX), "{msg}");
        // …and a WARN still proceeds there with its log, exactly as before.
        assert!(crate::resource_guard::admit_verdict(warn(), "x", false, None).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn an_admit_under_a_standing_critical_also_releases_at_the_deadline() {
        let out = admit_with(
            SpawnOrigin::LoopingAgent,
            "looping-agent terminal",
            "looping-agent:a",
            &cfg(),
            critical,
            no_drain,
            &GateState::new(),
        )
        .await;
        let Admitted::AfterHold { why, held } = out else {
            panic!("a standing CRITICAL must hold, got {out:?}");
        };
        assert_eq!(why, ReleaseWhy::HoldDeadline);
        assert!(held >= Duration::from_secs(600) && held <= Duration::from_secs(605));
    }

    // --- (e) operator origins never hold; Unknown does --------------------

    #[tokio::test(start_paused = true)]
    async fn operator_origins_never_hold_and_unknown_does() {
        for origin in [SpawnOrigin::OperatorTerminal, SpawnOrigin::OperatorChat] {
            let probed = AtomicU32::new(0);
            let t0 = Instant::now();
            let out = admit_with(
                origin,
                "terminal",
                "k",
                &cfg(),
                || {
                    probed.fetch_add(1, Ordering::SeqCst);
                    critical()
                },
                no_drain,
                &GateState::new(),
            )
            .await;
            assert_eq!(out, Admitted::Operator, "{origin}");
            assert_eq!(
                probed.load(Ordering::SeqCst),
                0,
                "{origin} must not even probe"
            );
            assert_eq!(Instant::now(), t0, "{origin} must not wait");
            // The public entry points agree, through the real classifier.
            admit(origin, "terminal", "k").await;
            assert_eq!(Instant::now(), t0);
            assert!(check_request(origin, "terminal", "k").is_ok());
            assert!(admit_or_defer(origin, "terminal", "k", Duration::ZERO)
                .await
                .is_ok());
            assert_eq!(load_precheck(origin, "terminal", "k"), Ok(()));
        }
        let out = admit_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "http_terminal:x",
            &cfg(),
            warn,
            no_drain,
            &GateState::new(),
        )
        .await;
        assert!(
            matches!(
                out,
                Admitted::AfterHold {
                    why: ReleaseWhy::HoldDeadline,
                    ..
                }
            ),
            "Unknown is autonomous (fail closed) and must hold: {out:?}"
        );
        let state = GateState::new();
        let refused = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &warn(),
            &cfg(),
            &state,
        );
        assert!(
            refused.is_err(),
            "Unknown must be refused at a request door"
        );
    }

    // --- (a) the 2026-10-27 burst, FIFO -----------------------------------

    /// 19 admits requested within 17 s (the densest burst measured on
    /// 2026-10-27: 20:30:24.9-20:30:41.3) under `Proceed` release on a >= 3 s
    /// grid, spanning >= 54 s (18 gaps x 3 s), in the order they asked.
    #[tokio::test(start_paused = true)]
    async fn the_2026_10_27_burst_of_19_in_17_seconds_is_paced_three_seconds_apart() {
        let state = Arc::new(GateState::new());
        let t0 = Instant::now();
        let mut tasks = Vec::new();
        for i in 0..19u64 {
            // Requests spread over 16.4 s — the measured window.
            let requested = t0 + Duration::from_millis(i * 16_400 / 18);
            let state = state.clone();
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep_until(requested).await;
                let key = format!("claim:pr:{i}");
                let _ = admit_with(
                    SpawnOrigin::GateContinuation,
                    "gate-continuation terminal",
                    &key,
                    &GateConfig::DEFAULT,
                    || SpawnGate::Proceed,
                    no_drain,
                    &state,
                )
                .await;
                (i, requested, Instant::now())
            }));
        }
        let mut released = Vec::new();
        for t in tasks {
            let (i, requested, at) = t.await.expect("admit task");
            assert!(at >= requested);
            assert!(requested <= t0 + Duration::from_secs(17));
            released.push((at, i));
        }
        released.sort();
        for pair in released.windows(2) {
            let gap = pair[1].0.saturating_duration_since(pair[0].0);
            assert!(gap >= Duration::from_secs(3), "two releases {gap:?} apart");
        }
        let order: Vec<u64> = released.iter().map(|(_, i)| *i).collect();
        assert_eq!(
            order,
            (0..19).collect::<Vec<_>>(),
            "released in arrival order"
        );
        let span = released[18].0.saturating_duration_since(released[0].0);
        assert!(span >= Duration::from_secs(54), "span {span:?}");
        let stats = &state.stats;
        assert_eq!(n(&stats.released_immediate) + n(&stats.released_pace), 19);
        assert_eq!(n(&stats.released_immediate), 1, "only the first is unpaced");
        assert_eq!(present(&state), 0);
    }

    #[test]
    fn pacing_holds_until_one_spacing_after_the_last_release_and_per_waiter_ahead() {
        let t0 = Instant::now();
        assert_eq!(
            decide(t0 + S, Some(t0), &SpawnGate::Proceed, None, &cfg()),
            GateStep::Hold(HoldWhy::Pace, t0 + Duration::from_secs(3))
        );
        assert_eq!(
            decide(t0 + 3 * S, Some(t0), &SpawnGate::Proceed, None, &cfg()),
            GateStep::Release(ReleaseWhy::Pace)
        );
        // Two waiters ahead: two more slots past the free one.
        assert_eq!(
            decide_in_queue(t0 + 3 * S, Some(t0), &SpawnGate::Proceed, None, 2, &cfg()),
            GateStep::Hold(HoldWhy::Pace, t0 + Duration::from_secs(9))
        );
        // A deadline release still waits for its pace slot.
        let t9 = t0 + Duration::from_secs(600);
        assert_eq!(
            decide(t9, Some(t9 - S), &warn(), Some(t0), &cfg()),
            GateStep::Hold(HoldWhy::Pace, t9 + 2 * S)
        );
        // Zero spacing switches pacing off.
        let off = GateConfig {
            spacing: Duration::ZERO,
            ..cfg()
        };
        assert_eq!(
            decide(t0, Some(t0), &SpawnGate::Proceed, None, &off),
            GateStep::Release(ReleaseWhy::Pace)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_zero_deadline_turns_holding_off_and_counts_no_deadline_release() {
        let state = GateState::new();
        let off = GateConfig {
            hold_deadline: Duration::ZERO,
            ..cfg()
        };
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "k",
            &off,
            warn,
            no_drain,
            &state,
        )
        .await;
        assert_eq!(out, Admitted::Immediate);
        assert_eq!(n(&state.stats.released_immediate), 1);
        assert_eq!(
            n(&state.stats.released_hold_deadline),
            0,
            "it was never held"
        );
    }

    // --- the drain (round-3 review B1) --------------------------------------

    /// B1: claimed work runs through coord's drain. A continuation held for load
    /// while the drain arms is released when the load clears — it never waits in
    /// the queue for the drain, where `/restart-readiness` would count it until
    /// the drain lifted.
    #[tokio::test(start_paused = true)]
    async fn under_an_armed_drain_a_held_claimed_continuation_is_released_not_held() {
        let state = GateState::new();
        let probes = AtomicU32::new(0);
        let out = tokio::time::timeout(
            Duration::from_secs(3_600),
            admit_with(
                SpawnOrigin::GateContinuation,
                "gate-continuation terminal",
                "gate:1",
                &cfg(),
                // Held for load across two rechecks; clear on the third reading.
                || {
                    if probes.fetch_add(1, Ordering::SeqCst) < 2 {
                        warn()
                    } else {
                        SpawnGate::Proceed
                    }
                },
                // The drain armed during the hold, and stays armed.
                || Some("coord has drained this device".to_string()),
                &state,
            ),
        )
        .await
        .expect("claimed work is never held for the drain");
        assert_eq!(
            out,
            Admitted::AfterHold {
                why: ReleaseWhy::Clear,
                held: Duration::from_secs(10)
            }
        );
        assert_eq!(present(&state), 0, "nothing is left waiting for the drain");
        assert_eq!(n(&state.stats.ran_through_drain), 1);
        assert_eq!(last_release(&state), Some(Instant::now()));
    }

    /// Round-4 review W1: a condition check runs through the drain too. Coord
    /// records its run `running` before it publishes the frame, and nothing ages
    /// that row out, so a check held for load while the drain arms is released
    /// when the load clears — deferring it would strand the run.
    #[tokio::test(start_paused = true)]
    async fn under_an_armed_drain_a_held_condition_check_is_released_not_deferred() {
        let state = GateState::new();
        let probes = AtomicU32::new(0);
        let out = tokio::time::timeout(
            Duration::from_secs(3_600),
            admit_with(
                SpawnOrigin::CoordDispatch,
                "condition-check terminal",
                "condition_run:1",
                &cfg(),
                || {
                    if probes.fetch_add(1, Ordering::SeqCst) < 1 {
                        warn()
                    } else {
                        SpawnGate::Proceed
                    }
                },
                || Some("device drained".to_string()),
                &state,
            ),
        )
        .await
        .expect("a condition check is never held for the drain");
        assert_eq!(
            out,
            Admitted::AfterHold {
                why: ReleaseWhy::Clear,
                held: Duration::from_secs(5)
            },
            "released, not deferred"
        );
        assert_eq!(state.held(), 0);
        assert_eq!(n(&state.stats.ran_through_drain), 1);
        assert_eq!(state.health_json(&cfg())["ranThroughDrainTotal"], 1);
    }

    // --- (f) the same work key admitted twice is paced twice --------------

    #[tokio::test(start_paused = true)]
    async fn the_same_work_key_admitted_twice_is_paced_twice() {
        let state = GateState::new();
        let t0 = Instant::now();
        for _ in 0..2 {
            let _ = admit_with(
                SpawnOrigin::GateContinuation,
                "gate-continuation terminal",
                "claim:pr:same",
                &cfg(),
                || SpawnGate::Proceed,
                no_drain,
                &state,
            )
            .await;
        }
        assert_eq!(
            Instant::now().saturating_duration_since(t0),
            Duration::from_secs(3),
            "the second admission of one key waits a full slot behind the first"
        );
        assert_eq!(n(&state.stats.released_immediate), 1);
        assert_eq!(n(&state.stats.released_pace), 1);
    }

    // --- (g) request doors -------------------------------------------------

    /// A request door under WARN is refused `machine_loaded` with the re-probe
    /// interval as its retry hint and claims neither a slot nor a place in the
    /// queue; once the episode has stood for the deadline the door is admitted
    /// too (W8) — a human-facing door is not locked out by a standing WARN.
    #[tokio::test(start_paused = true)]
    async fn a_request_door_under_warn_refuses_machine_loaded_until_the_episode_deadline() {
        let state = GateState::new();
        let refused = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &warn(),
            &cfg(),
            &state,
        )
        .err()
        .expect("WARN must refuse at a request door");
        assert_eq!(refused.code(), "machine_loaded");
        assert_eq!(refused.why, HoldWhy::Warn);
        assert_eq!(refused.retry_after_ms, 5_000);
        assert!(refused.reason.contains("262 threads"), "{}", refused.reason);
        assert!(!refused.reason.contains("unattended"), "{}", refused.reason);
        let body = serde_json::to_value(refused.api_refusal()).expect("body");
        assert_eq!(body["code"], "machine_loaded");
        assert_eq!(body["success"], false);
        assert_eq!(body["hint"]["retry_after_ms"], 5_000);
        assert_eq!(n(&state.stats.refused_warn), 1);
        assert_eq!(last_release(&state), None, "a refusal claims no slot");
        assert_eq!(present(&state), 0, "a refusal takes no ticket");

        tokio::time::advance(Duration::from_secs(600)).await;
        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &warn(),
            &cfg(),
            &state,
        )
        .expect("the episode is past the deadline: the door is admitted");
        slot.commit().await;
        assert_eq!(n(&state.stats.released_hold_deadline), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_door_never_jumps_a_queued_waiter() {
        let state = GateState::new();
        let waiter = state.enqueue();
        let refused = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .err()
        .expect("a queued waiter goes first");
        assert_eq!(refused.why, HoldWhy::Pace);
        assert_eq!(refused.retry_after_ms, 3_000, "one slot behind the waiter");
        state.leave(waiter);
        assert!(check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .is_ok());
    }

    /// W7: a door that refuses after the gate (a bad workingDir, a refused
    /// grant) drops its slot, and the drop refunds it; a committed slot stays.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_request_slot_is_refunded_and_a_committed_one_is_kept() {
        let state = GateState::new();
        let t0 = Instant::now();
        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .expect("admitted");
        assert_eq!(last_release(&state), Some(t0));
        drop(slot);
        assert_eq!(last_release(&state), None, "refunded");
        assert_eq!(n(&state.stats.refunded), 1);
        assert_eq!(n(&state.stats.released_immediate), 0);

        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .expect("admitted again: the refund freed the slot");
        slot.commit().await;
        assert_eq!(last_release(&state), Some(t0), "kept");
        assert_eq!(n(&state.stats.released_immediate), 1);
    }

    /// Round-3 N6: a door's PTY opens seconds after its claim. A commit that
    /// finds another spawn released after the claim waits out the rest of one
    /// spacing behind it; either way the cursor moves to the PTY's real opening.
    #[tokio::test(start_paused = true)]
    async fn committing_a_request_slot_paces_the_pty_that_actually_opens() {
        // Overtaken: claim at t0, a queued waiter releases at t0+3, the door's
        // worktree add finishes at t0+3.1 — its PTY must wait to t0+6.
        let state = Arc::new(GateState::new());
        let t0 = Instant::now();
        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .expect("admitted");
        let waiter = {
            let state = state.clone();
            tokio::spawn(async move {
                let _ = admit_with(
                    SpawnOrigin::GateContinuation,
                    "gate-continuation terminal",
                    "claim:pr:q",
                    &GateConfig::DEFAULT,
                    || SpawnGate::Proceed,
                    no_drain,
                    &state,
                )
                .await;
                Instant::now()
            })
        };
        tokio::time::sleep_until(t0 + Duration::from_millis(3_100)).await;
        assert_eq!(waiter.await.expect("waiter"), t0 + 3 * S);
        slot.commit().await;
        assert_eq!(Instant::now(), t0 + 6 * S, "one spacing behind the waiter");
        assert_eq!(last_release(&state), Some(t0 + 6 * S));

        // Not overtaken: no wait, and the cursor still moves to the PTY's opening
        // (t1+3.1, not the claim at t1), so the next spawn is paced from there.
        let state = GateState::new();
        let t1 = Instant::now();
        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .expect("admitted");
        tokio::time::advance(Duration::from_millis(3_100)).await;
        slot.commit().await;
        assert_eq!(Instant::now(), t1 + Duration::from_millis(3_100));
        assert_eq!(
            last_release(&state),
            Some(t1 + Duration::from_millis(3_100))
        );
    }

    /// Round-3 review W1: a door's overtaken commit waits without a ticket, so a
    /// queued waiter whose slot is the commit's wake could open beside it. The
    /// commit is counted ahead of the queue while it waits, and the head yields.
    #[tokio::test(start_paused = true)]
    async fn a_committing_door_and_the_next_queued_waiter_open_a_spacing_apart() {
        let state = GateState::new();
        let t0 = Instant::now();
        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .expect("the door claims t0");
        let (w1, w2) = (state.enqueue(), state.enqueue());

        // W1 is released at t0+3, after the door's claim: it overtakes it.
        tokio::time::advance(3 * S).await;
        let step = state.step(&SpawnGate::Proceed, Some(w1), &mut Holds::default(), &cfg());
        assert_eq!(step.step, GateStep::Release(ReleaseWhy::Pace));
        let w1_at = Instant::now();

        // The door's worktree add ends at t0+3.1: its commit waits to t0+6. At
        // t0+3.2 W2 is the queue head, and its slot would be t0+6 — the commit's
        // wake. Both run on this task, ordered by the clock.
        let door = async {
            tokio::time::sleep_until(t0 + Duration::from_millis(3_100)).await;
            slot.commit().await;
            Instant::now()
        };
        let head = async {
            tokio::time::sleep_until(t0 + Duration::from_millis(3_200)).await;
            assert_eq!(state.lock().committing, 1, "the commit is waiting");
            let mut holds = Holds::default();
            let step = state.step(&SpawnGate::Proceed, Some(w2), &mut holds, &cfg());
            assert_eq!(
                step.step,
                GateStep::Hold(HoldWhy::Pace, t0 + 9 * S),
                "the queue head yields one spacing to the committing door"
            );
            let mut at = t0 + 9 * S;
            loop {
                tokio::time::sleep_until(at).await;
                match state
                    .step(&SpawnGate::Proceed, Some(w2), &mut holds, &cfg())
                    .step
                {
                    GateStep::Release(_) => break,
                    GateStep::Hold(_, next) => at = next,
                }
            }
            Instant::now()
        };
        let (door_at, w2_at) = tokio::join!(door, head);
        assert_eq!(state.lock().committing, 0);
        let mut opens = vec![w1_at, door_at, w2_at];
        opens.sort();
        for pair in opens.windows(2) {
            let gap = pair[1].saturating_duration_since(pair[0]);
            assert!(gap >= 3 * S, "two PTYs opened {gap:?} apart: {opens:?}");
        }
    }

    /// Round-4 review nit 1: two slow doors. X claims, then Y claims after X's
    /// slot; X's commit is overtaken by Y's claim and waits one spacing behind
    /// it. Y, committing while X waits, must not open inside that spacing: it
    /// waits one spacing behind X's scheduled opening.
    #[tokio::test(start_paused = true)]
    async fn two_slow_doors_open_their_ptys_a_spacing_apart() {
        fn claim(state: &GateState) -> RequestSlot<'_> {
            check_request_with(
                SpawnOrigin::Unknown,
                "HTTP terminal",
                "k",
                &SpawnGate::Proceed,
                &cfg(),
                state,
            )
            .expect("admitted")
        }
        let state = GateState::new();
        let t0 = Instant::now();
        let x = claim(&state);
        tokio::time::advance(3 * S).await;
        let y = claim(&state);
        assert_eq!(last_release(&state), Some(t0 + 3 * S), "Y claimed at t0+3");
        // X's worktree add ends at t0+3.1, Y's at t0+3.2.
        let (x_at, y_at) = tokio::join!(
            async {
                tokio::time::sleep_until(t0 + Duration::from_millis(3_100)).await;
                x.commit().await;
                Instant::now()
            },
            async {
                tokio::time::sleep_until(t0 + Duration::from_millis(3_200)).await;
                assert_eq!(state.lock().committing_until, Some(t0 + 6 * S));
                y.commit().await;
                Instant::now()
            },
        );
        let gap = y_at.saturating_duration_since(x_at);
        assert!(
            gap >= 3 * S,
            "two PTYs opened {gap:?} apart: X {x_at:?}, Y {y_at:?}"
        );
        assert_eq!((x_at, y_at), (t0 + 6 * S, t0 + 9 * S));
        assert_eq!(state.lock().committing, 0);
        assert_eq!(state.lock().committing_until, None);
    }

    /// A commit future cancelled mid-wait un-counts itself, so it never delays
    /// the queue forever.
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_commit_stops_counting_ahead_of_the_queue() {
        let state = GateState::new();
        let slot = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            &SpawnGate::Proceed,
            &cfg(),
            &state,
        )
        .expect("admitted");
        let w = state.enqueue();
        tokio::time::advance(3 * S).await;
        let _ = state.step(&SpawnGate::Proceed, Some(w), &mut Holds::default(), &cfg());
        // Overtaken, so the commit waits to t0+6; it is dropped at t0+3.1.
        let cancelled =
            tokio::time::timeout(Duration::from_millis(100), async { slot.commit().await }).await;
        assert!(cancelled.is_err(), "the commit was still waiting");
        assert_eq!(state.lock().committing, 0);
        assert_eq!(state.lock().committing_until, None);
    }

    #[test]
    fn the_gate_joins_the_one_spawn_admission_object_as_a_field() {
        let merged = with_gate_block(
            serde_json::json!({ "verdict": "proceed", "reserved": 2 }),
            serde_json::json!({ "heldNow": 1 }),
        );
        assert_eq!(merged["verdict"], "proceed", "#2429's fields are kept");
        assert_eq!(merged["reserved"], 2);
        assert_eq!(merged["unattendedGate"]["heldNow"], 1);
        // A non-object block is never dropped.
        let odd = with_gate_block(serde_json::json!(null), serde_json::json!({ "heldNow": 0 }));
        assert!(odd["admission"].is_null() && odd["unattendedGate"].is_object());
    }

    // --- deferrable sites (W5) ---------------------------------------------

    /// Several pending respawns in one catch-up pass drain at one per spacing:
    /// each waits inline for its slot instead of deferring to the next tick.
    #[tokio::test(start_paused = true)]
    async fn pending_respawns_drain_at_one_per_spacing_within_one_pass() {
        let state = GateState::new();
        let t0 = Instant::now();
        for i in 0..3 {
            let key = format!("respawn:{i}");
            let admitted = admit_or_defer_with(
                SpawnOrigin::Respawn,
                "respawned session",
                &key,
                UNATTENDED_SPAWN_SPACING,
                &cfg(),
                || SpawnGate::Proceed,
                &state,
            )
            .await;
            assert!(admitted.is_ok(), "respawn {i}");
            assert_eq!(Instant::now(), t0 + 3 * S * i, "respawn {i} at its slot");
        }
        assert_eq!(n(&state.stats.deferred_pace), 0);
    }

    /// A deferred key keeps its ticket: when its caller retries it goes ahead of
    /// a waiter that queued after it, while a brand-new key still waits behind.
    #[tokio::test(start_paused = true)]
    async fn a_deferred_key_keeps_its_place_in_the_queue() {
        let state = GateState::new();
        let deferred = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            "respawn:a",
            Duration::ZERO,
            &cfg(),
            warn,
            &state,
        )
        .await
        .expect_err("WARN defers");
        assert_eq!(deferred.why, HoldWhy::Warn);
        assert_eq!(deferred.retry_after_ms, 5_000);
        assert_eq!(present(&state), 0, "a deferred caller leaves the queue");
        assert_eq!(state.lock().deferred.len(), 1);

        // A task waiter queues after the deferral.
        let later = state.enqueue();
        // A new key is behind that waiter…
        let new_key = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            "respawn:new",
            Duration::ZERO,
            &cfg(),
            || SpawnGate::Proceed,
            &state,
        )
        .await;
        assert_eq!(new_key.map(drop).map_err(|l| l.why), Err(HoldWhy::Pace));
        // …but the remembered key is ahead of it, and is released.
        let retried = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            "respawn:a",
            Duration::ZERO,
            &cfg(),
            || SpawnGate::Proceed,
            &state,
        )
        .await;
        assert!(retried.is_ok());
        assert!(
            !state.lock().deferred.contains_key("respawn:a"),
            "a release forgets it"
        );
        assert!(
            state.lock().present.contains(&later),
            "the later waiter is still queued"
        );
    }

    /// Round-3 N2: a kept ticket competing with waiters that are actively
    /// releasing. With a zero budget it defers again at every retry while the
    /// backlog drains; with one spacing it waits for the next slot and takes it
    /// ahead of every waiter that queued after it.
    #[tokio::test(start_paused = true)]
    async fn a_kept_ticket_with_one_spacing_wins_its_turn_against_releasing_waiters() {
        let state = Arc::new(GateState::new());
        let key = "looping_agent:merge-shepherd";
        // The agent deferred under WARN: its ticket (the lowest) is kept.
        let first = admit_or_defer_with(
            SpawnOrigin::LoopingAgent,
            "looping-agent terminal",
            key,
            UNATTENDED_SPAWN_SPACING,
            &cfg(),
            warn,
            &state,
        )
        .await;
        assert!(matches!(first, Err(ref l) if l.why == HoldWhy::Warn));
        // A backlog of four continuations queues after it and starts releasing.
        let t0 = Instant::now();
        let mut backlog = Vec::new();
        for i in 0..4u32 {
            let state = state.clone();
            backlog.push(tokio::spawn(async move {
                let key = format!("claim:pr:{i}");
                let _ = admit_with(
                    SpawnOrigin::GateContinuation,
                    "gate-continuation terminal",
                    &key,
                    &GateConfig::DEFAULT,
                    || SpawnGate::Proceed,
                    no_drain,
                    &state,
                )
                .await;
                Instant::now()
            }));
        }
        // The supervisor's tick lands between two releases (t0+4).
        tokio::time::sleep_until(t0 + 4 * S).await;
        let zero = admit_or_defer_with(
            SpawnOrigin::LoopingAgent,
            "looping-agent terminal",
            key,
            Duration::ZERO,
            &cfg(),
            || SpawnGate::Proceed,
            &state,
        )
        .await;
        assert!(
            matches!(zero, Err(ref l) if l.why == HoldWhy::Pace),
            "a zero budget defers while the backlog drains: {zero:?}"
        );
        let one = admit_or_defer_with(
            SpawnOrigin::LoopingAgent,
            "looping-agent terminal",
            key,
            UNATTENDED_SPAWN_SPACING,
            &cfg(),
            || SpawnGate::Proceed,
            &state,
        )
        .await;
        assert!(one.is_ok(), "one spacing is enough for the kept ticket");
        let agent_at = Instant::now();
        assert_eq!(agent_at, t0 + 6 * S, "the next slot after t0+3");
        let mut releases = Vec::new();
        for task in backlog {
            releases.push(task.await.expect("backlog"));
        }
        releases.sort();
        assert_eq!(
            releases,
            vec![t0, t0 + 3 * S, t0 + 9 * S, t0 + 12 * S],
            "the two still-queued waiters went after the agent"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn remembered_deferrals_are_evicted_by_age_and_counted_honestly() {
        let state = GateState::new();
        for key in ["respawn:warn", "respawn:pace"] {
            let verdict = if key.ends_with("warn") {
                warn()
            } else {
                SpawnGate::Proceed
            };
            if key.ends_with("pace") {
                // Something is queued ahead, so this one defers for pace.
                let _ahead = state.enqueue();
            }
            let _ = admit_or_defer_with(
                SpawnOrigin::Respawn,
                "respawned session",
                key,
                Duration::ZERO,
                &cfg(),
                || verdict.clone(),
                &state,
            )
            .await;
        }
        let health = state.health_json(&cfg());
        assert_eq!(
            health["deferredNow"], 2,
            "load AND pace deferrals are counted"
        );
        assert_eq!(health["deferredTotal"]["warn"], 1);
        assert_eq!(health["deferredTotal"]["pace"], 1);
        tokio::time::advance(Duration::from_secs(601)).await;
        assert_eq!(
            state.health_json(&cfg())["deferredNow"],
            0,
            "not retried within the TTL: forgotten"
        );
    }

    /// Round-3 review nit: a remembered key's load clocks end with the load. A
    /// key whose clock passed its deadline in one episode is held again, on a
    /// fresh clock, by a later unrelated episode.
    #[tokio::test(start_paused = true)]
    async fn a_remembered_key_is_not_exempt_from_a_later_episode() {
        let state = GateState::new();
        let t0 = Instant::now();
        let key = "respawn:stale";
        let read = |verdict: SpawnGate| {
            drop(check_request_with(
                SpawnOrigin::Unknown,
                "x",
                "k",
                &verdict,
                &cfg(),
                &state,
            ));
        };
        // A WARN episode from t0, kept alive past its deadline.
        read(warn());
        let ticket = state.enqueue();
        state.leave(ticket);
        tokio::time::advance(Duration::from_secs(590)).await;
        read(warn());
        tokio::time::advance(Duration::from_secs(60)).await;
        // Deferred (for pace) with a load clock from an episode at t0, now past
        // its deadline.
        state.remember(
            key,
            ticket,
            Holds {
                load: Some(t0),
                critical: None,
            },
            Instant::now(),
            &cfg(),
        );
        // The load clears…
        read(SpawnGate::Proceed);
        assert_eq!(
            state.lock().deferred.get(key).map(|r| r.holds),
            Some(Holds::default()),
            "the clear cleared the remembered clocks"
        );
        // …and later a new, unrelated WARN episode starts, read by someone else.
        tokio::time::advance(Duration::from_secs(5)).await;
        read(warn());
        tokio::time::advance(Duration::from_secs(5)).await;
        let retried = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            key,
            Duration::ZERO,
            &cfg(),
            warn,
            &state,
        )
        .await;
        assert!(
            matches!(retried, Err(ref l) if l.why == HoldWhy::Warn),
            "the new episode holds the key on a fresh clock: {retried:?}"
        );

        // With no live episode at the retry (one nobody observed for a whole
        // deadline), the kept clocks are not carried into the next one either.
        let state = GateState::new();
        let t1 = Instant::now();
        assert!(admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            key,
            Duration::ZERO,
            &cfg(),
            warn,
            &state,
        )
        .await
        .is_err());
        tokio::time::advance(Duration::from_secs(590)).await;
        state.lock().deferred.get_mut(key).expect("kept").last_seen = Instant::now();
        tokio::time::advance(Duration::from_secs(20)).await;
        assert!(
            state.lock().episode.is_some_and(|e| e.since == t1),
            "fixture: the episode from t1, unobserved since"
        );
        let retried = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            key,
            Duration::ZERO,
            &cfg(),
            warn,
            &state,
        )
        .await;
        assert!(
            matches!(retried, Err(ref l) if l.why == HoldWhy::Warn),
            "held afresh: {retried:?}"
        );
    }

    /// A door that refuses after `admit_or_defer` released (a spent account-hop
    /// budget) gives the slot back; a release that is kept stays.
    #[tokio::test(start_paused = true)]
    async fn a_released_slot_refunded_before_the_pty_frees_the_next_spawn() {
        let state = GateState::new();
        let t0 = Instant::now();
        let released = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            "respawn:cap",
            Duration::ZERO,
            &cfg(),
            || SpawnGate::Proceed,
            &state,
        )
        .await
        .expect("released");
        assert_eq!(last_release(&state), Some(t0));
        released.refund();
        assert_eq!(last_release(&state), None, "refunded");
        assert_eq!(n(&state.stats.refunded), 1);
        let kept = admit_or_defer_with(
            SpawnOrigin::Respawn,
            "respawned session",
            "respawn:next",
            Duration::ZERO,
            &cfg(),
            || SpawnGate::Proceed,
            &state,
        )
        .await;
        assert!(kept.is_ok(), "the refund freed the slot: {kept:?}");
        drop(kept);
        assert_eq!(last_release(&state), Some(t0), "a dropped release is kept");

        // `admit`'s release too: a continuation whose create failed (the seam's
        // CRITICAL refusal after a `hold_deadline` release) gives its slot back.
        let state = GateState::new();
        let (_, released) = admit_released(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "gate:refund",
            &cfg(),
            || SpawnGate::Proceed,
            no_drain,
            &state,
        )
        .await;
        assert_eq!(last_release(&state), Some(Instant::now()));
        released.refund();
        assert_eq!(last_release(&state), None, "refunded");
        assert_eq!(n(&state.stats.refunded), 1);
    }

    #[test]
    fn the_load_precheck_defers_under_load_without_claiming_anything() {
        let state = GateState::new();
        let loaded = load_precheck_with(
            SpawnOrigin::Respawn,
            "respawned session",
            "respawn:s",
            &warn(),
            &cfg(),
            &state,
        )
        .expect_err("WARN defers before any work");
        assert_eq!(loaded.retry_after_ms, 5_000);
        assert_eq!(last_release(&state), None);
        assert_eq!(present(&state), 0);
        assert_eq!(n(&state.stats.deferred_warn), 1);
        assert_eq!(
            load_precheck_with(
                SpawnOrigin::Respawn,
                "respawned session",
                "respawn:s",
                &SpawnGate::Proceed,
                &cfg(),
                &state,
            ),
            Ok(())
        );
        assert_eq!(
            last_release(&state),
            None,
            "a pre-check never claims a slot"
        );
    }

    #[test]
    fn a_machine_loaded_refusal_is_recognisable_by_its_prefix() {
        let state = GateState::new();
        let loaded =
            check_request_with(SpawnOrigin::Unknown, "x", "k", &critical(), &cfg(), &state)
                .err()
                .expect("refused");
        assert!(is_machine_loaded_refusal(&loaded.refusal_text()));
        assert!(!is_machine_loaded_refusal(
            "resource_guard:critical:thread_count: no"
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn the_health_block_names_every_counter() {
        let state = GateState::new();
        state.stats.note_release(false, ReleaseWhy::Pace);
        state.stats.note_release(true, ReleaseWhy::Pace);
        state.stats.note_refusal(HoldWhy::Critical);
        let _queued = state.enqueue();
        let j = state.health_json(&GateConfig::DEFAULT);
        assert_eq!(j["spacingMs"], 3_000);
        assert_eq!(j["recheckMs"], 5_000);
        assert_eq!(j["holdDeadlineSecs"], 600);
        assert_eq!(j["heldNow"], 1);
        assert_eq!(j["deferredNow"], 0);
        assert!(j["loadEpisode"].is_null());
        assert_eq!(j["releasedTotal"]["immediate"], 1);
        assert_eq!(j["releasedTotal"]["pace"], 1);
        assert_eq!(j["releasedTotal"]["clear"], 0);
        assert_eq!(j["releasedTotal"]["holdDeadline"], 0);
        assert_eq!(j["refusedTotal"]["critical"], 1);
        assert_eq!(j["deferredTotal"]["warn"], 0);
        assert_eq!(j["ranThroughDrainTotal"], 0);
        assert_eq!(j["refundedTotal"], 0);
        // The live block is the same shape.
        assert!(health_json()["releasedTotal"].is_object());
    }
}
