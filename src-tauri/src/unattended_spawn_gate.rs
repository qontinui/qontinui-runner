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
//!    pacer, not a second one. The thread reading behind the probe is memoized
//!    for 250 ms (`health_monitor::THREAD_READING_TTL`), so a spawn paced 3 s
//!    behind its predecessor reads a number that already includes the
//!    predecessor's reader/waiter threads. That is what breaks "19 starts on one
//!    reading".
//! 2. **Holds** while [`crate::resource_guard::probe_for_spawn`] reads WARN or
//!    CRITICAL, re-probing every [`UNATTENDED_RECHECK_INTERVAL`] (5 s), up to
//!    [`UNATTENDED_HOLD_DEADLINE`] (10 min). At the deadline it releases with
//!    `why=hold_deadline` and the seam's existing ladder applies unchanged: WARN
//!    proceeds with its log, CRITICAL refuses. The hold is bounded because a hold
//!    forever under a standing WARN would silently stop all unattended work,
//!    which served policy ranks worse than spawning at WARN.
//!
//! The thresholds are the ones [`crate::resource_guard::effective_thread_ceilings`]
//! already folds (and the memory floors beside them) — the gate reads the probe's
//! verdict and never a number of its own. It ships enabled with no product
//! toggle; the two env overrides below are operator tuning knobs (`0` spacing
//! turns pacing off, `0` deadline turns holding off).
//!
//! ## Three entry points, by the shape of the caller
//!
//! * [`admit`] — **task-owned** sites (a continuation, a condition check, a
//!   looping agent, a steward restart, a resume batch). Awaits: holds and paces.
//! * [`check_request`] — **request-scoped** doors (`POST /terminals`, the relay's
//!   remote create, the `terminal_create` proxy). Calls [`decide`] ONCE and never
//!   awaits a hold: a held HTTP request is a timed-out request. A `Hold` verdict
//!   is answered as the door's drain arm answers, with the code
//!   [`MACHINE_LOADED_CODE`] and a `retry_after_ms`.
//! * [`admit_or_defer`] — sites that run INLINE on coord's session-handoff `/ws`
//!   read loop (respawn and handoff materialisation). Holding there would stop
//!   the socket being read — pings, and the attach/create grant arms on the same
//!   socket — for up to the deadline, so they defer instead, exactly as their
//!   drain arm does: the durable coord row replays on the next catch-up tick
//!   (60 s). The first deferral of a work key is remembered, so the deadline
//!   still bounds a standing WARN for them.
//!
//! Operator origins (`!SpawnOrigin::is_autonomous()`) pass every entry point
//! untouched; `SpawnOrigin::Unknown` is autonomous (fail closed).
//!
//! ## Log grammar
//!
//! ```text
//! resource_guard: holding unattended <what> origin=<origin> work=<key> lane=<lane> observed=<n> limit=<n> why=<warn|critical|pace>
//! resource_guard: releasing unattended <what> origin=<origin> work=<key> held_ms=<n> why=<clear|hold_deadline|pace>
//! ```
//!
//! A pace hold reports `lane=pace observed=<ms since the last unattended
//! release> limit=<spacing ms>`. A spawn that never waited logs nothing. A
//! holding line is written when the hold's `why` changes, not on every recheck.
//!
//! ## Composition
//!
//! [`decide`] is PURE so the headless launch seam (open PR #2207's
//! `admit_headless_spawn`) can compose it instead of growing a second hold state
//! machine: whichever of the two lands second collapses onto this one.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
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

/// The longest a load hold lasts before it releases with `why=hold_deadline`.
pub(crate) const UNATTENDED_HOLD_DEADLINE: Duration = Duration::from_secs(600);

/// The refusal code a request-scoped door answers a `Hold` verdict with.
pub(crate) const MACHINE_LOADED_CODE: &str = "machine_loaded";

/// Upper bounds on the env overrides. Not policy: an `Instant + Duration` that
/// overflows PANICS, and nothing on the path to a PTY may be able to unwind.
const MAX_SPACING: Duration = Duration::from_secs(3_600);
const MAX_DEADLINE: Duration = Duration::from_secs(7 * 24 * 3_600);

/// How long one probe verdict is shared between waiters. Every waiter re-probes
/// on each wake, and N waiters contending for one pace slot all wake together;
/// sharing the verdict for a second bounds the probe (and the second-lane line
/// `probe_for_spawn` logs when both lanes trip) to about one per second however
/// many spawns are held. Shorter than [`UNATTENDED_SPAWN_SPACING`], so each paced
/// spawn still reads a verdict taken after its predecessor was released.
const PROBE_MEMO_TTL: Duration = Duration::from_millis(1_000);

/// Most work keys [`admit_or_defer`] remembers a first deferral for. A key is
/// forgotten when it is released; this bounds keys whose work never returned.
const DEFERRED_KEYS_MAX: usize = 256;

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
    /// Another unattended spawn was released less than one spacing ago.
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
    /// It was held for load past [`GateConfig::hold_deadline`]; the seam's own
    /// ladder decides from here.
    HoldDeadline,
    /// It was never held for load; at most it waited for its pace slot.
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

/// PURE: the gate's verdict for one spawn at `now`.
///
/// * `last_slot` — the instant the last unattended spawn was released (the
///   process-global cursor), `None` before the first.
/// * `verdict` — [`crate::resource_guard::probe_for_spawn`]'s reading.
/// * `held_since` — when THIS spawn was first held for load, `None` if it has
///   not been. The deadline is measured from it.
///
/// Load first: WARN or CRITICAL holds until a recheck, never past the deadline
/// (the recheck instant is capped at it, so a deadline release lands exactly on
/// the deadline). Past the deadline, or under `Proceed`, pacing decides: the
/// slot is [`next_release_slot`] after `last_slot`, and a slot in the future is a
/// pace hold until it.
pub(crate) fn decide(
    now: Instant,
    last_slot: Option<Instant>,
    verdict: &SpawnGate,
    held_since: Option<Instant>,
    cfg: &GateConfig,
) -> GateStep {
    let load = match verdict {
        SpawnGate::Proceed => None,
        SpawnGate::Warn(_) => Some(HoldWhy::Warn),
        SpawnGate::Critical(_) => Some(HoldWhy::Critical),
    };
    let mut why = if held_since.is_some() {
        ReleaseWhy::Clear
    } else {
        ReleaseWhy::Pace
    };
    if let Some(load_why) = load {
        let since = held_since.unwrap_or(now);
        let deadline = since.checked_add(cfg.hold_deadline);
        if deadline.is_none_or(|d| now < d) {
            let recheck = now.checked_add(cfg.recheck).unwrap_or(now);
            let at = deadline.map_or(recheck, |d| recheck.min(d));
            return GateStep::Hold(load_why, at);
        }
        why = ReleaseWhy::HoldDeadline;
    }
    let slot = next_release_slot(now, last_slot, cfg.spacing, Duration::ZERO);
    if slot > now {
        return GateStep::Hold(HoldWhy::Pace, slot);
    }
    GateStep::Release(why)
}

// ---------------------------------------------------------------------------
// Process-global state and counters
// ---------------------------------------------------------------------------

/// The slot cursor: when the last unattended spawn was released.
static SLOT: Mutex<Option<Instant>> = Mutex::new(None);

/// [`admit_or_defer`]'s memory: work key -> its first load deferral.
static DEFERRED: Mutex<BTreeMap<String, Instant>> = Mutex::new(BTreeMap::new());

/// The shared probe verdict and when it was taken (see [`PROBE_MEMO_TTL`]).
static PROBE_MEMO: Mutex<Option<(Instant, SpawnGate)>> = Mutex::new(None);

static STATS: GateStats = GateStats::new();

/// The `/health` counters. Totals are since process start.
#[derive(Debug)]
pub(crate) struct GateStats {
    held_now: AtomicUsize,
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
}

impl GateStats {
    pub(crate) const fn new() -> Self {
        Self {
            held_now: AtomicUsize::new(0),
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

    fn to_json(&self, cfg: &GateConfig, deferred_now: usize) -> serde_json::Value {
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        serde_json::json!({
            "spacingMs": u64::try_from(cfg.spacing.as_millis()).unwrap_or(u64::MAX),
            "recheckMs": u64::try_from(cfg.recheck.as_millis()).unwrap_or(u64::MAX),
            "holdDeadlineSecs": cfg.hold_deadline.as_secs(),
            "heldNow": self.held_now.load(Ordering::Relaxed),
            "deferredNow": deferred_now,
            "releasedTotal": {
                "immediate": n(&self.released_immediate),
                "pace": n(&self.released_pace),
                "clear": n(&self.released_clear),
                "holdDeadline": n(&self.released_hold_deadline),
            },
            "refusedTotal": {
                "warn": n(&self.refused_warn),
                "critical": n(&self.refused_critical),
                "pace": n(&self.refused_pace),
            },
            "deferredTotal": {
                "warn": n(&self.deferred_warn),
                "critical": n(&self.deferred_critical),
                "pace": n(&self.deferred_pace),
            },
        })
    }
}

/// Counts one spawn as held for as long as it lives — dropped on release AND
/// when the awaiting future is cancelled, so `heldNow` never leaks.
struct HeldGuard<'a>(&'a AtomicUsize);

impl<'a> HeldGuard<'a> {
    fn new(gauge: &'a AtomicUsize) -> Self {
        gauge.fetch_add(1, Ordering::Relaxed);
        Self(gauge)
    }
}

impl Drop for HeldGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `/health` `spawnAdmission.unattendedGate`.
pub(crate) fn health_json() -> serde_json::Value {
    let deferred_now = DEFERRED.lock().unwrap_or_else(PoisonError::into_inner).len();
    STATS.to_json(&GateConfig::from_env(), deferred_now)
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

fn log_releasing(
    what: &str,
    origin: SpawnOrigin,
    work_key: &str,
    held: Duration,
    why: ReleaseWhy,
) {
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

/// What [`admit_with`] did. Test-facing; [`admit`] discards it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// An operator origin: the gate does not apply.
    Operator,
    /// Released on the first decision, without waiting.
    Immediate,
    /// Released after waiting `held` in total.
    AfterHold { why: ReleaseWhy, held: Duration },
}

/// Hold and pace one unattended PTY spawn. Await it IMMEDIATELY before the PTY
/// create, after every other await on the path, so the verdict it releases on
/// is the one the PTY opens under.
///
/// Returns at once for an operator origin. Otherwise it never fails: it returns
/// when the spawn may proceed (load clear and pace slot free, or the hold
/// deadline passed). The seam's ladder still runs at the PTY create.
pub(crate) async fn admit(origin: SpawnOrigin, what: &str, work_key: &str) {
    let _ = admit_with(
        origin,
        what,
        work_key,
        &GateConfig::from_env(),
        memoized_probe,
        &SLOT,
        &STATS,
    )
    .await;
}

/// [`admit`] over an injected probe, cursor and counters.
pub(crate) async fn admit_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    cfg: &GateConfig,
    mut probe: impl FnMut() -> SpawnGate,
    cursor: &Mutex<Option<Instant>>,
    stats: &GateStats,
) -> Admitted {
    if !origin.is_autonomous() {
        return Admitted::Operator;
    }
    let started = Instant::now();
    let mut held_since: Option<Instant> = None;
    let mut logged: Option<HoldWhy> = None;
    let mut guard: Option<HeldGuard<'_>> = None;
    loop {
        let now = Instant::now();
        let verdict = probe();
        let (step, last_slot) = {
            let mut slot = cursor.lock().unwrap_or_else(PoisonError::into_inner);
            let last_slot = *slot;
            let step = decide(now, last_slot, &verdict, held_since, cfg);
            if matches!(step, GateStep::Release(_)) {
                *slot = Some(now);
            }
            (step, last_slot)
        };
        match step {
            GateStep::Release(why) => {
                let waited = guard.is_some();
                stats.note_release(waited, why);
                if !waited {
                    return Admitted::Immediate;
                }
                let held = now.saturating_duration_since(started);
                log_releasing(what, origin, work_key, held, why);
                return Admitted::AfterHold { why, held };
            }
            GateStep::Hold(why, at) => {
                if why != HoldWhy::Pace && held_since.is_none() {
                    held_since = Some(now);
                }
                if logged != Some(why) {
                    let fields = hold_fields(why, &verdict, now, last_slot, cfg);
                    log_holding("holding", what, origin, work_key, why, &fields, "");
                    logged = Some(why);
                }
                if guard.is_none() {
                    guard = Some(HeldGuard::new(&stats.held_now));
                }
                tokio::time::sleep_until(at).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request-scoped doors and the /ws-loop sites: decide once
// ---------------------------------------------------------------------------

/// A `Hold` verdict answered without waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MachineLoaded {
    pub(crate) why: HoldWhy,
    /// The next pace slot, or the re-probe interval for a load hold.
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
                "unattended spawns are paced one per {} ms",
                cfg.spacing.as_millis()
            ),
        };
        Self {
            why,
            retry_after_ms,
            reason: format!(
                "machine loaded: {cause} — not starting this unattended {what} now; retry in \
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
}

/// The ONE decision both decide-once entry points take: release (claiming the
/// slot) or a [`MachineLoaded`].
fn decide_once(
    now: Instant,
    verdict: &SpawnGate,
    held_since: Option<Instant>,
    what: &str,
    cfg: &GateConfig,
    cursor: &Mutex<Option<Instant>>,
) -> Result<ReleaseWhy, (MachineLoaded, Option<Instant>)> {
    let mut slot = cursor.lock().unwrap_or_else(PoisonError::into_inner);
    let last_slot = *slot;
    match decide(now, last_slot, verdict, held_since, cfg) {
        GateStep::Release(why) => {
            *slot = Some(now);
            Ok(why)
        }
        GateStep::Hold(why, at) => Err((
            MachineLoaded::new(why, at, now, verdict, what, cfg),
            last_slot,
        )),
    }
}

/// The request-scoped door's gate: [`decide`] ONCE, never a wait. `Err` is
/// answered as the door's drain arm answers, with [`MACHINE_LOADED_CODE`].
///
/// Call it where the door's drain arm sits — before any worktree allocation or
/// grant consumption a refusal would otherwise leak.
pub(crate) fn check_request(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
) -> Result<(), MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    check_request_with(
        origin,
        what,
        work_key,
        Instant::now(),
        &memoized_probe(),
        &GateConfig::from_env(),
        &SLOT,
        &STATS,
    )
}

/// [`check_request`] over an injected clock, verdict, cursor and counters.
pub(crate) fn check_request_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    now: Instant,
    verdict: &SpawnGate,
    cfg: &GateConfig,
    cursor: &Mutex<Option<Instant>>,
    stats: &GateStats,
) -> Result<(), MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    match decide_once(now, verdict, None, what, cfg, cursor) {
        Ok(why) => {
            stats.note_release(false, why);
            Ok(())
        }
        Err((loaded, last_slot)) => {
            stats.note_refusal(loaded.why);
            let fields = hold_fields(loaded.why, verdict, now, last_slot, cfg);
            let tail = format!(" retry_after_ms={}", loaded.retry_after_ms);
            log_holding("refusing", what, origin, work_key, loaded.why, &fields, &tail);
            Err(loaded)
        }
    }
}

/// Which way a site that serves both kinds of caller waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wait {
    /// A task-owned caller: [`admit`].
    Hold,
    /// A request-scoped caller: [`check_request`].
    Refuse,
}

/// [`admit`] or [`check_request`], by `wait` — for a site shared by a task and
/// a request handler (`mcp::steward::start_steward`).
pub(crate) async fn admit_or_refuse(
    wait: Wait,
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
) -> Result<(), MachineLoaded> {
    match wait {
        Wait::Hold => {
            admit(origin, what, work_key).await;
            Ok(())
        }
        Wait::Refuse => check_request(origin, what, work_key),
    }
}

/// The gate for a site that runs inline on coord's session-handoff `/ws` read
/// loop and whose work is a durable coord row that replays on the loop's
/// catch-up tick. Decides once; `Err` is a DEFERRAL, answered as the site's
/// drain arm answers (the row stays pending). The first load deferral of
/// `work_key` is remembered, so a standing WARN still releases it at the hold
/// deadline instead of deferring it forever.
pub(crate) fn admit_or_defer(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
) -> Result<(), MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    admit_or_defer_with(
        origin,
        what,
        work_key,
        Instant::now(),
        &memoized_probe(),
        &GateConfig::from_env(),
        &SLOT,
        &DEFERRED,
        &STATS,
    )
}

/// [`admit_or_defer`] over an injected clock, verdict, cursor, memory and
/// counters.
pub(crate) fn admit_or_defer_with(
    origin: SpawnOrigin,
    what: &str,
    work_key: &str,
    now: Instant,
    verdict: &SpawnGate,
    cfg: &GateConfig,
    cursor: &Mutex<Option<Instant>>,
    deferred: &Mutex<BTreeMap<String, Instant>>,
    stats: &GateStats,
) -> Result<(), MachineLoaded> {
    if !origin.is_autonomous() {
        return Ok(());
    }
    let held_since = deferred
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(work_key)
        .copied();
    match decide_once(now, verdict, held_since, what, cfg, cursor) {
        Ok(why) => {
            deferred
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(work_key);
            stats.note_release(held_since.is_some(), why);
            if let Some(since) = held_since {
                log_releasing(what, origin, work_key, now.saturating_duration_since(since), why);
            }
            Ok(())
        }
        Err((loaded, last_slot)) => {
            if loaded.why != HoldWhy::Pace && held_since.is_none() {
                let mut memory = deferred.lock().unwrap_or_else(PoisonError::into_inner);
                // Forget the oldest first deferral when full: its work most
                // likely never came back, and losing it costs only a deadline
                // restart, never a spawn.
                if memory.len() >= DEFERRED_KEYS_MAX {
                    if let Some(oldest) = memory
                        .iter()
                        .min_by_key(|(_, at)| **at)
                        .map(|(k, _)| k.clone())
                    {
                        memory.remove(&oldest);
                    }
                }
                memory.insert(work_key.to_string(), now);
            }
            stats.note_deferral(loaded.why);
            let fields = hold_fields(loaded.why, verdict, now, last_slot, cfg);
            let tail = format!(
                " retry_after_ms={} (deferred; the durable row replays on catch-up)",
                loaded.retry_after_ms
            );
            log_holding("holding", what, origin, work_key, loaded.why, &fields, &tail);
            Err(loaded)
        }
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

    fn cursor() -> Mutex<Option<Instant>> {
        Mutex::new(None)
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
        let stats = GateStats::new();
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:1",
            &cfg(),
            probe,
            &cursor(),
            &stats,
        )
        .await;
        assert_eq!(
            out,
            Admitted::AfterHold {
                why: ReleaseWhy::Clear,
                held: Duration::from_secs(10)
            }
        );
        assert_eq!(stats.released_clear.load(Ordering::SeqCst), 1);
        assert_eq!(stats.held_now.load(Ordering::SeqCst), 0);
    }

    // --- (c) a standing WARN releases at the deadline ---------------------

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
        let stats = GateStats::new();
        let out = admit_with(
            SpawnOrigin::GateContinuation,
            "gate-continuation terminal",
            "claim:pr:2",
            &cfg(),
            warn,
            &cursor(),
            &stats,
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
        assert_eq!(stats.released_hold_deadline.load(Ordering::SeqCst), 1);
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
            &cursor(),
            &GateStats::new(),
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
                &cursor(),
                &GateStats::new(),
            )
            .await;
            assert_eq!(out, Admitted::Operator, "{origin}");
            assert_eq!(probed.load(Ordering::SeqCst), 0, "{origin} must not even probe");
            assert_eq!(Instant::now(), t0, "{origin} must not wait");
            // The public entry points agree, through the real classifier.
            admit(origin, "terminal", "k").await;
            assert_eq!(Instant::now(), t0);
            assert_eq!(check_request(origin, "terminal", "k"), Ok(()));
            assert_eq!(admit_or_defer(origin, "terminal", "k"), Ok(()));
        }
        let out = admit_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "http_terminal:x",
            &cfg(),
            warn,
            &cursor(),
            &GateStats::new(),
        )
        .await;
        assert!(
            matches!(out, Admitted::AfterHold { why: ReleaseWhy::HoldDeadline, .. }),
            "Unknown is autonomous (fail closed) and must hold: {out:?}"
        );
        let refused = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "http_terminal:x",
            Instant::now(),
            &warn(),
            &cfg(),
            &cursor(),
            &GateStats::new(),
        );
        assert!(refused.is_err(), "Unknown must be refused at a request door");
    }

    // --- (a) the 2026-10-27 burst -----------------------------------------

    /// 19 admits requested within 17 s (the densest burst measured on
    /// 2026-10-27: 20:30:24.9-20:30:41.3) under `Proceed` release on a >= 3 s
    /// grid, spanning >= 54 s (18 gaps x 3 s).
    #[tokio::test(start_paused = true)]
    async fn the_2026_10_27_burst_of_19_in_17_seconds_is_paced_three_seconds_apart() {
        let cursor = Arc::new(cursor());
        let stats = Arc::new(GateStats::new());
        let t0 = Instant::now();
        let mut tasks = Vec::new();
        for i in 0..19u64 {
            // Requests spread over 16.4 s — the measured window.
            let requested = t0 + Duration::from_millis(i * 16_400 / 18);
            let (cursor, stats) = (cursor.clone(), stats.clone());
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep_until(requested).await;
                let key = format!("claim:pr:{i}");
                let _ = admit_with(
                    SpawnOrigin::GateContinuation,
                    "gate-continuation terminal",
                    &key,
                    &GateConfig::DEFAULT,
                    || SpawnGate::Proceed,
                    &cursor,
                    &stats,
                )
                .await;
                (requested, Instant::now())
            }));
        }
        let mut released = Vec::new();
        for t in tasks {
            let (requested, at) = t.await.expect("admit task");
            assert!(at >= requested);
            assert!(requested <= t0 + Duration::from_secs(17));
            released.push(at);
        }
        released.sort();
        for pair in released.windows(2) {
            let gap = pair[1].saturating_duration_since(pair[0]);
            assert!(gap >= Duration::from_secs(3), "two releases {gap:?} apart");
        }
        let span = released[18].saturating_duration_since(released[0]);
        assert!(span >= Duration::from_secs(54), "span {span:?}");
        let n = |c: &AtomicU64| c.load(Ordering::SeqCst);
        assert_eq!(n(&stats.released_immediate) + n(&stats.released_pace), 19);
        assert_eq!(n(&stats.released_immediate), 1, "only the first is unpaced");
    }

    #[test]
    fn pacing_holds_until_one_spacing_after_the_last_release() {
        let t0 = Instant::now();
        assert_eq!(
            decide(t0 + S, Some(t0), &SpawnGate::Proceed, None, &cfg()),
            GateStep::Hold(HoldWhy::Pace, t0 + Duration::from_secs(3))
        );
        assert_eq!(
            decide(t0 + 3 * S, Some(t0), &SpawnGate::Proceed, None, &cfg()),
            GateStep::Release(ReleaseWhy::Pace)
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

    // --- (f) the same work key admitted twice is paced twice --------------

    #[tokio::test(start_paused = true)]
    async fn the_same_work_key_admitted_twice_is_paced_twice() {
        let cursor = cursor();
        let stats = GateStats::new();
        let t0 = Instant::now();
        for _ in 0..2 {
            let _ = admit_with(
                SpawnOrigin::GateContinuation,
                "gate-continuation terminal",
                "claim:pr:same",
                &cfg(),
                || SpawnGate::Proceed,
                &cursor,
                &stats,
            )
            .await;
        }
        assert_eq!(
            Instant::now().saturating_duration_since(t0),
            Duration::from_secs(3),
            "the second admission of one key waits a full slot behind the first"
        );
        assert_eq!(stats.released_immediate.load(Ordering::SeqCst), 1);
        assert_eq!(stats.released_pace.load(Ordering::SeqCst), 1);
    }

    // --- (g) a request-scoped site refuses machine_loaded without waiting -

    #[tokio::test(start_paused = true)]
    async fn a_request_scoped_site_under_warn_returns_machine_loaded_without_waiting() {
        let cursor = cursor();
        let stats = GateStats::new();
        let t0 = Instant::now();
        let refused = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "http_terminal:x",
            t0,
            &warn(),
            &cfg(),
            &cursor,
            &stats,
        )
        .expect_err("WARN must refuse at a request door");
        assert!(Instant::now().saturating_duration_since(t0) < UNATTENDED_SPAWN_SPACING);
        assert_eq!(refused.code(), "machine_loaded");
        assert_eq!(refused.why, HoldWhy::Warn);
        assert_eq!(refused.retry_after_ms, 5_000);
        assert!(refused.reason.contains("262 threads"), "{}", refused.reason);
        let body = serde_json::to_value(refused.api_refusal()).expect("body");
        assert_eq!(body["code"], "machine_loaded");
        assert_eq!(body["success"], false);
        assert_eq!(body["hint"]["retry_after_ms"], 5_000);
        assert_eq!(stats.refused_warn.load(Ordering::SeqCst), 1);
        assert_eq!(*cursor.lock().expect("cursor"), None, "a refusal claims no slot");

        // Clear: released, and the release claims the slot…
        assert_eq!(
            check_request_with(
                SpawnOrigin::Unknown,
                "HTTP terminal",
                "k",
                t0,
                &SpawnGate::Proceed,
                &cfg(),
                &cursor,
                &stats,
            ),
            Ok(())
        );
        assert_eq!(*cursor.lock().expect("cursor"), Some(t0));
        // …so a second request one second later is refused for pace, with the
        // remaining two seconds as its retry hint.
        let paced = check_request_with(
            SpawnOrigin::Unknown,
            "HTTP terminal",
            "k",
            t0 + S,
            &SpawnGate::Proceed,
            &cfg(),
            &cursor,
            &stats,
        )
        .expect_err("pace");
        assert_eq!(paced.why, HoldWhy::Pace);
        assert_eq!(paced.retry_after_ms, 2_000);
    }

    // --- the /ws-loop deferral ------------------------------------------

    #[test]
    fn a_deferred_key_is_remembered_so_a_standing_warn_still_releases_at_the_deadline() {
        let cursor = cursor();
        let memory = Mutex::new(BTreeMap::new());
        let stats = GateStats::new();
        let t0 = Instant::now();
        let call = |now: Instant, verdict: &SpawnGate| {
            admit_or_defer_with(
                SpawnOrigin::Respawn,
                "respawned session",
                "respawn:s",
                now,
                verdict,
                &cfg(),
                &cursor,
                &memory,
                &stats,
            )
        };
        let first = call(t0, &warn()).expect_err("WARN defers");
        assert_eq!(first.retry_after_ms, 5_000);
        assert_eq!(memory.lock().expect("m").get("respawn:s"), Some(&t0));
        // A retry one catch-up tick later is still inside the deadline.
        assert!(call(t0 + 60 * S, &warn()).is_err());
        assert_eq!(memory.lock().expect("m").get("respawn:s"), Some(&t0));
        // At the deadline, measured from the FIRST deferral, it is released.
        assert_eq!(call(t0 + 600 * S, &warn()), Ok(()));
        assert!(memory.lock().expect("m").is_empty(), "a release forgets the key");
        assert_eq!(stats.released_hold_deadline.load(Ordering::SeqCst), 1);
        assert_eq!(stats.deferred_warn.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_health_block_names_every_counter() {
        let stats = GateStats::new();
        stats.note_release(false, ReleaseWhy::Pace);
        stats.note_release(true, ReleaseWhy::Pace);
        stats.note_refusal(HoldWhy::Critical);
        let j = stats.to_json(&GateConfig::DEFAULT, 2);
        assert_eq!(j["spacingMs"], 3_000);
        assert_eq!(j["recheckMs"], 5_000);
        assert_eq!(j["holdDeadlineSecs"], 600);
        assert_eq!(j["heldNow"], 0);
        assert_eq!(j["deferredNow"], 2);
        assert_eq!(j["releasedTotal"]["immediate"], 1);
        assert_eq!(j["releasedTotal"]["pace"], 1);
        assert_eq!(j["releasedTotal"]["clear"], 0);
        assert_eq!(j["releasedTotal"]["holdDeadline"], 0);
        assert_eq!(j["refusedTotal"]["critical"], 1);
        assert_eq!(j["deferredTotal"]["warn"], 0);
        // The live block is the same shape.
        assert!(health_json()["releasedTotal"].is_object());
    }
}
