//! Spawn-time resource gate — the last thing consulted before the runner
//! creates a **new** session (a PTY, or a secondary runner process).
//!
//! Plan `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D.
//!
//! ## What this exists to prevent
//!
//! Overnight 2026-08-06→07 several Claude Code sessions running inside
//! runner-spawned terminals died while their terminal windows stayed open and
//! the runner process itself never restarted. Windows' Resource-Exhaustion-
//! Detector fired 12 times between 00:00 and 05:29, each firing naming
//! `vmmemWSL` (up to ~17 GB) plus concurrent `rustc.exe` / `clippy-driver.exe`.
//! That is commit-charge exhaustion: Windows kills whatever process is
//! mid-allocation when the ceiling is hit, while the parent shell — which is
//! not allocating — survives untouched. The runner's own `terminal::session`
//! lifecycle logged **zero** closes in that window, because nothing local was
//! watching.
//!
//! So this gate fires at exactly one moment: **the instant a new thing is about
//! to be spawned.** It never touches, throttles, pauses or closes anything that
//! is already alive — the fleet's standing `runner-lifecycle` doctrine (never
//! stop/kill a live runner or session) applied to the one place a *new* one gets
//! created. Adding a session is a choice; the sessions already running are not.
//!
//! ## Pure over injected inputs
//!
//! [`evaluate`] takes a reading and a settings struct and returns a verdict. It
//! probes nothing and reads no globals, which is what makes the thresholds
//! arguable in a test rather than in production. [`probe_for_spawn`] is the thin
//! live wrapper that supplies the two inputs. This is deliberately the shape
//! [`crate::ci_node::admission`] already uses — `headroom_defers` is split out
//! of `admission_decision` for exactly this reason (`ci_node/admission.rs`,
//! "Split out from `admission_decision` so the threshold policy can be exercised
//! on its own").
//!
//! ## Three terms, only tightening, and two clamps
//!
//! The floors [`evaluate`] compares against are
//! `max(local override, cached fleet default, hardcoded default)` — see
//! [`merge_floors`]. A machine owner can only ever TIGHTEN their own protection,
//! never loosen it, which is the same non-loosening discipline this fleet
//! applies to policy-clause edits.
//!
//! A `max` of three independently-authored terms can, however, produce a
//! configuration none of the three asked for, so the fold is followed by two
//! clamps that bound what the composition may say:
//!
//! - [`SESSION_FLOOR_MAX_BYTES`] caps every effective floor. This lane fails
//!   CLOSED on eight of its ten seams (no override, no timeout to fail open
//!   through), so an unreachable floor is not a stricter guard — it is a machine,
//!   or a whole tenant, that can never start a session again.
//! - [`coerce_ladder`] then forces `critical <= warn`. Folding the two floors
//!   independently lets a fleet row that states only the critical column invert
//!   the ladder — every warn becomes a refusal and the warn band ceases to exist
//!   — which is exactly the state
//!   [`crate::commands::resource_guard_settings`] refuses to persist locally
//!   ("a machine would block a spawn it had never warned about"). What a local
//!   writer refuses to store, a remote term must not be able to synthesise.
//!
//! The fleet term is a **cached, best-effort refinement**: it comes from
//! [`crate::mcp::fleet_policy_poller`]'s 45 s background loop, read
//! synchronously out of a process-global cache. **The spawn path never calls
//! coord.** That is not an optimisation, it is the requirement — this gate
//! exists to protect sessions on a machine under load, which is exactly when a
//! coord round-trip is least likely to answer, and a guard that degraded to "no
//! warning" when coord was unreachable would be missing on precisely the nights
//! it is for. With an empty cache the effective floor is
//! `max(local, hardcoded)`, exactly as it was before the poller existed.
//!
//! ## Two lanes, and a direction that inverts
//!
//! The gate reads two sensors, not one, and they disagree about which way is
//! bad. Free commit is a **floor** — lower is worse. The runner's own OS thread
//! count is a **ceiling** — higher is worse. Everything the ceiling lane does is
//! the mirror of what the floor lane does, and the mirror is spelled out at each
//! site so a future reader does not "correct" it back: the three-term fold is a
//! `min` ([`tighten_ceiling`]) rather than a `max` ([`tighten`]); the clamp that
//! keeps the composition livable pushes UP to [`THREAD_CEILING_MIN`] rather than
//! down to [`SESSION_FLOOR_MAX_BYTES`]; the ladder invariant is
//! `critical >= warn` ([`coerce_ceiling_ladder`]) rather than `critical <= warn`
//! ([`coerce_ladder`]); and the verdict boundary is strictly ABOVE rather than
//! strictly below. The *model* is identical — three terms that may only tighten,
//! two clamps, a three-valued verdict, fail open on UNKNOWN — which is why there
//! is one composition model here and not two.
//!
//! ## Why a thread lane at all
//!
//! On 2026-08-29 the primary runner wedged carrying **540 OS threads**, 119 of
//! them inside `CreateProcess`, against tokio's default `max_blocking_threads`
//! of **512**. The root cause (an untimed WMI call leaking blocking-pool
//! threads) is fixed elsewhere; the aggravating factor is this plan's: a burst
//! of ~130 concurrent session spawns landed on an already-loaded machine with
//! nothing to slow it down. The free-commit floor structurally could not see it
//! — the box had memory, it had run out of threads — so a gate that consults
//! only that floor would have admitted every one of those spawns again.
//!
//! ## Fail OPEN, always
//!
//! `commit_available_bytes()` returns `Option`, and so does
//! [`crate::health_monitor::thread_count_reading`]. `None` means the sensor is
//! UNKNOWN, UNKNOWN means this gate has no opinion, and no opinion means
//! **proceed** — see [`SpawnGate::Proceed`]. The thread sensor's `None` arm is
//! why that function exists at all: `get_thread_count()` renders an unreadable
//! sensor as `0`, and against a CEILING `0` is not a missing reading, it is the
//! most reassuring number the type can hold. Every other guard in this fleet's
//! ladder takes the same posture (`ci_node/admission.rs`'s `Headroom` doc:
//! "an unreadable sensor is UNKNOWN, and unknown means no headroom opinion at
//! all (fail open)"). The whole failure mode of a guard like this must be false
//! negatives — a missed warning — never a false positive that blocks the
//! operator's actual work on a telemetry gap.
//!
//! ## Host lane only, and the smallest reading that answers the question
//!
//! The reading comes from
//! [`crate::fleet::resource_sample::spawn_gate_reading`]: the host lane's name
//! and its free-commit figure, and nothing else. Those are the only two values
//! [`evaluate`] consults, and taking only them is not a micro-optimisation — it
//! is a property this seam needs. `TerminalSession::spawn` is called
//! SYNCHRONOUSLY on a tokio worker from every unattended spawn seam
//! (`mcp::terminals`, `mcp::steward`, `mcp::tauri_proxy`, `mcp::backend_relay`,
//! and both `session::transport` seams), so whatever this gate touches, a
//! runtime worker waits for. The publisher's full
//! `collect_host_lane()` additionally refreshes sysinfo, enumerates EVERY volume
//! on the box (including disconnected network and removable drives, which block
//! for as long as the OS takes to give up), reads the `ci_node` settings and
//! computes build occupancy — none of which this verdict reads, and any of which
//! can park a worker on a stalled mount. `available_commit_bytes()` is one
//! `GlobalMemoryStatusEx` call: microseconds, no allocation, no volume probe.
//!
//! The publisher still sends the full sample, so the gate and the fleet
//! dashboard still agree on the *quantity* — plan §A3's converged free-commit
//! number, read through the same function — taken at two instants. Two instants
//! is all a spawn-time verdict could honestly claim anyway: the published row is
//! up to 30 s old by the time a PTY opens.
//!
//! The host lane is also the *correct* lane (§Part A step 3): the WSL probe
//! forks `wsl.exe` under a 5 s timeout, and a pre-PTY gate that can stall five
//! seconds on a cold-starting WSL VM is a worse user-facing failure than the one
//! it prevents. With `pageReporting=true` the host free-commit figure already
//! nets out WSL's live usage, so it is not blind to `vmmemWSL`; it is precisely
//! the quantity that collapsed to 7.25 GB during the incident.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter};
use tracing::warn;

use crate::fleet::resource_sample::Lane;
use crate::mcp::fleet_policy_poller::SessionFloors;
use crate::settings::SessionGuardSettings;

/// Tauri event carrying a resource-guard observation to the webview.
///
/// Consumed by `src/hooks/useResourceGuardNotifications.ts`, which turns it into
/// a toast on the runner's general toast system (`useToast` + `ToastContainer`).
/// Emitted for the WARN verdict and for a CRITICAL verdict that an override let
/// through — **not** for a CRITICAL refusal, which travels back to the caller as
/// the typed `Err` below and is surfaced by the blocking dialog (or, on an
/// unattended path, by that path's own error reporting). Emitting both would
/// stack a self-dismissing toast on top of the modal that is asking the operator
/// to decide.
pub(crate) const RESOURCE_GUARD_EVENT: &str = "resource-guard-notice";

/// Prefix on the `Err` string a CRITICAL refusal returns.
///
/// The spawn seams this gate lives on (`TerminalSession::spawn`,
/// `InstanceManager::launch_instance_with_app`) both signal failure as
/// `Result<_, String>`, and widening those to a typed error enum would ripple
/// through a dozen unrelated call sites for no gain. A stable prefix keeps the
/// refusal machine-recognisable end to end: `src/lib/resourceGuard.ts` matches
/// on it to decide "this is an overridable refusal, show the dialog" versus
/// "this is a real spawn failure, report it". Everything after the prefix is
/// human text and may be reworded freely; the prefix may not.
pub(crate) const CRITICAL_REFUSAL_PREFIX: &str = "resource_guard:critical:";

/// One gibibyte, the unit the floors are quoted in.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Ceiling on every effective session floor, in bytes (12 GiB).
///
/// ## Why this lane needs an upper bound at all
///
/// [`merge_floors`] is a `max` over three terms, none of which is bounded at its
/// source: the local override is any `u64` a hand-edited `settings.json` cares
/// to name, the panel's own input accepts up to 128 GiB, and the fleet column is
/// a `BIGINT` whose only validation is its sign — coord's `validate()` rejects a
/// negative, not an absurd positive, so `i64::MAX` decodes straight through
/// [`crate::mcp::fleet_policy_poller`]'s `floor_bytes`.
///
/// An unreachable floor here does not make the guard stricter, it makes the
/// machine unusable. [`crate::ci_node::admission::MAX_SESSION_DEFER_FLOOR_GB`]
/// argues the same point for the CI lane and this lane is the worse of the two:
/// CI *defers*, retries on a 60 s waker and can be re-homed to another host,
/// whereas an un-overridden CRITICAL verdict is a hard refusal with no timeout to
/// fail open through, and eight of this gate's ten seams are unattended
/// (`resource_override = false`) with nobody to press "Start anyway". Via the
/// fleet column one bad row would do it to every machine in the tenant, forever.
///
/// ## Why 12 GiB, and not 8 or 16
///
/// The bound has to be wide enough that an operator can still express "this box
/// needs a lot of headroom", and narrow enough that nothing they can express
/// makes the box unreachable:
///
/// - **Not lower than 12.** The effective floor computed here is ALSO what
///   `ci_node` admission consumes (`probe_headroom` reads
///   [`effective_session_floors`], and [`crate::ci_node::admission::defer_commit_floor_gb`]
///   clamps it at `MAX_SESSION_DEFER_FLOOR_GB` = 12 GiB). Capping below that
///   would silently delete a shipped behaviour: with an 8 GiB cap,
///   `max(DEFER_FREE_COMMIT_GB, min(floor, 12))` is 8 for every setting, and the
///   session term could never widen the CI defer band at all — the exact
///   "not tight, but empty" failure `MAX_SESSION_DEFER_FLOOR_GB`'s own doc
///   rejects. One rung of this ladder is [`crate::ci_node::admission::MIN_FREE_COMMIT_GB`]
///   (4 GiB), so 12 GiB is also 4× the shipped warn default and 8× the shipped
///   critical default.
/// - **Not higher than 12.** These boxes have 32 GB of physical RAM and
///   `.wslconfig memory=16GB`, so a single resident WSL VM can hold half the
///   machine on its own. A floor at or above 16 GiB could therefore be
///   unclearable while WSL is merely *resident* — not busy — which is a
///   permanently-closed gate rather than a strict one. At 12 GiB the box always
///   recovers when the build that ate the headroom finishes: the 2026-08-06→07
///   incident bottomed out at 7.25 GB free commit with `rustc` and `vmmemWSL`
///   both live, and idle free commit here sits in the tens of GB.
///
/// The panel's `SESSION_FLOOR_MAX_GIB = 128` is a *typing* bound, not a policy
/// one — 128 GiB is above the entire commit limit of every box on this fleet
/// (71.71 GB here) — which is why the enforcing side needs its own.
pub(crate) const SESSION_FLOOR_MAX_BYTES: u64 = 12 * 1024 * 1024 * 1024;

/// Lower bound on every effective thread ceiling, in OS threads (200).
///
/// The mirror of [`SESSION_FLOOR_MAX_BYTES`], and it exists for the identical
/// reason read in the other direction: [`tighten_ceiling`] is a `min` over three
/// terms, none of which is bounded at its source — a hand-edited `settings.json`
/// names any `usize`, and the fleet column (when coord grows one) is a `BIGINT`
/// whose only validation is its sign. An unreachably LOW ceiling is not a
/// stricter guard, it is a machine that can never start a session again: eight
/// of this gate's ten seams are unattended, with nobody to press "Start anyway",
/// and via the fleet term one bad row would do it to every machine in the tenant
/// at once, forever.
///
/// ## Why 200, measured rather than guessed
///
/// A ceiling is only reachable if the runner can sit BELOW it while doing
/// nothing, so the bound has to clear the at-rest thread count of a real runner.
/// **Measured 2026-08-30**, sampling `/proc/<pid>/task` every 3 s against the
/// live runner on the Linux dev box (debug build, embedded Postgres, full bridge
/// set): a steady **150-151** threads with no session running. That is the
/// number this constant has to beat, and it is 20 higher than the 100-130 band
/// [`crate::health_monitor::THREAD_WARNING_THRESHOLD`]'s doc still quotes.
///
/// - **Not 64**, the round number this was first proposed at, and not 128 or
///   150 either: all three sit at or below a measured idle process. Clamped
///   there, every reading on the box is already over the critical ceiling, and
///   the clamp meant to GUARANTEE spawnability becomes the thing that removes
///   it — the exact failure it exists to prevent, delivered by the mechanism
///   meant to prevent it.
/// - **Not higher than 200.** It has to stay strictly below both shipped
///   defaults (256 warn / 400 critical) or it pins a knob: a clamp equal to the
///   warn default would make the warn ceiling untunable in both directions
///   (the `min` fold already forbids loosening it), which is the "not tight, but
///   empty" failure [`SESSION_FLOOR_MAX_BYTES`]'s own doc rejects on the other
///   lane. At 200 the tunable ranges are warn `[200, 256]` and critical
///   `[200, 400]`, both non-empty.
///
/// 200 is therefore ~33% of headroom above the measured idle count and ~22%
/// below the shipped warn ceiling. A machine clamped all the way down to it
/// still starts sessions: 151 is not above 200.
pub(crate) const THREAD_CEILING_MIN: usize = 200;

/// What a lane measures, and therefore which direction is bad.
///
/// This exists because a verdict has to be able to describe either lane
/// HONESTLY. Before it, the verdict carried `free_bytes` / `floor_bytes` and
/// rendered both through [`format_gib`] — field names and a unit that would each
/// be a lie on the thread lane, and lies that no reviewer would catch, because
/// `412` formats as `0.00 GiB` perfectly happily. Carrying the metric with the
/// numbers is what lets one message template serve both lanes with no per-lane
/// branching at any call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneMetric {
    /// Free commit bytes — LOWER is worse; the limit is a FLOOR.
    FreeCommitBytes,
    /// The runner process's OS thread count — HIGHER is worse; the limit is a
    /// CEILING.
    ThreadCount,
}

impl LaneMetric {
    /// Stable machine name for the event payload
    /// (`src/hooks/useResourceGuardNotifications.ts`). Snake case to match the
    /// Rust field names it stands in for; the webview only ever compares it,
    /// never renders it.
    fn wire_name(self) -> &'static str {
        match self {
            LaneMetric::FreeCommitBytes => "free_commit_bytes",
            LaneMetric::ThreadCount => "thread_count",
        }
    }

    /// Opening words of the operator-facing notice — what KIND of pressure this
    /// is, before any number. "Low memory" on a box with 40 GB free but 500
    /// threads would send the operator to close a build that was never the
    /// problem.
    fn headline(self) -> &'static str {
        match self {
            LaneMetric::FreeCommitBytes => "Low memory",
            LaneMetric::ThreadCount => "High thread count",
        }
    }

    /// A reading as a standalone quantity: `"1.42 GiB"`, `"412 threads"`.
    fn quantity(self, value: u64) -> String {
        match self {
            LaneMetric::FreeCommitBytes => format_gib(value),
            LaneMetric::ThreadCount => format!("{value} threads"),
        }
    }

    /// A limit used ATTRIBUTIVELY, i.e. in front of "warn floor" / "warn
    /// ceiling": `"1.42 GiB"`, `"150-thread"`. English needs the singular
    /// hyphenated form there ("the 150-thread warn ceiling"), and quoting
    /// "the 150 threads warn ceiling" in a message whose whole job is to be
    /// actionable is the kind of wrongness that makes an operator distrust the
    /// number next to it.
    fn attributive(self, limit: u64) -> String {
        match self {
            LaneMetric::FreeCommitBytes => format_gib(limit),
            LaneMetric::ThreadCount => format!("{limit}-thread"),
        }
    }

    /// The noun for the limit: a floor is crossed downwards, a ceiling upwards.
    fn limit_noun(self) -> &'static str {
        match self {
            LaneMetric::FreeCommitBytes => "floor",
            LaneMetric::ThreadCount => "ceiling",
        }
    }

    /// What the operator can actually DO. A refusal that says only "not enough
    /// resources" gives them nothing to act on, and the two lanes have
    /// genuinely different answers: freeing memory does not return a thread to
    /// the blocking pool.
    fn remedy(self) -> &'static str {
        match self {
            LaneMetric::FreeCommitBytes => {
                "Free memory (close a build or a session) and try again, or start anyway to \
                 override."
            }
            LaneMetric::ThreadCount => {
                "Let some running sessions finish (or close a few) and try again, or start anyway \
                 to override."
            }
        }
    }
}

/// One lane's reading beside the limit it was judged against.
///
/// Carries the lane NAME as well as the metric because the two memory lanes
/// (`host`, `wsl`) share a metric and must never be confused for one another,
/// and because the name is what the fleet-limit cache is keyed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateObservation {
    /// [`Lane::as_str`] — `"host"`, `"wsl"` or `"threads"`. Never a literal.
    pub(crate) lane: String,
    pub(crate) metric: LaneMetric,
    /// What was measured, in the metric's own unit.
    pub(crate) observed: u64,
    /// The floor it fell below, or the ceiling it rose above.
    pub(crate) limit: u64,
}

impl GateObservation {
    /// The reading as the operator should read it: `"1.42 GiB"`,
    /// `"412 threads"`.
    pub(crate) fn observed_display(&self) -> String {
        self.metric.quantity(self.observed)
    }

    /// The limit as the operator should read it, in the same standalone form.
    pub(crate) fn limit_display(&self) -> String {
        self.metric.quantity(self.limit)
    }

    /// The whole situation as one clause, phrased in the metric's own direction:
    ///
    /// - `"the host lane has 2.00 GiB of free commit, below the 3.00 GiB warn floor"`
    /// - `"the runner process is carrying 412 threads, above the 150-thread warn ceiling"`
    ///
    /// `severity` is the word that names WHICH limit ("warn" / "critical"). The
    /// per-metric branching lives here and only here, which is the point of the
    /// type: [`admit_spawn`], [`precheck_spawn`] and [`critical_refusal`] all
    /// compose their messages out of this clause without knowing which lane
    /// spoke.
    pub(crate) fn clause(&self, severity: &str) -> String {
        let limit = self.metric.attributive(self.limit);
        let noun = self.metric.limit_noun();
        match self.metric {
            LaneMetric::FreeCommitBytes => format!(
                "the {} lane has {} of free commit, below the {limit} {severity} {noun}",
                self.lane,
                self.observed_display(),
            ),
            LaneMetric::ThreadCount => format!(
                "the runner process is carrying {}, above the {limit} {severity} {noun}",
                self.observed_display(),
            ),
        }
    }
}

/// Verdict of the spawn gate.
///
/// Deliberately three-valued rather than a bool. The three states carry
/// different *verdicts*, not different quantities — the same distinction the
/// rest of this fleet's ladder is built on (`cargo-guard.sh` defers at 5 GiB,
/// the supervisor's build pool defers at 5 GiB, `ci_node` hard-rejects at
/// 4 GiB, and all three read the same Windows free-commit number). A warn is
/// the lightest verdict in that ladder, which is why its floor sits lowest but
/// one; a block on a human's own spawn is the heaviest, which is why its floor
/// is lowest of all and why it is always overridable.
///
/// The payload is a [`GateObservation`] rather than a byte pair so the same
/// three verdicts describe the thread lane without renaming a field into a lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnGate {
    /// Enough headroom — or no readable opinion at all. Spawn.
    Proceed,
    /// Past the warn limit. Spawn anyway, but tell the operator: the point of
    /// the warning is that they can free the resource *before* the next spawn,
    /// and blocking here would be a heavier verdict than the evidence supports.
    Warn(GateObservation),
    /// Past the critical limit. Refuse by default, and let an explicit
    /// override through — a false positive here blocks the operator's actual
    /// work, which is a worse failure than an occasional missed warning.
    Critical(GateObservation),
}

impl SpawnGate {
    /// Order the three verdicts so two lanes can be composed. Proceed 0, Warn
    /// 1, Critical 2 — heavier wins, which is the only ordering a guard may
    /// use: the alternative is a lane that measured a refusal being talked out
    /// of it by a lane that measured nothing.
    fn severity(&self) -> u8 {
        match self {
            SpawnGate::Proceed => 0,
            SpawnGate::Warn(_) => 1,
            SpawnGate::Critical(_) => 2,
        }
    }

    /// The severity WORD and the observation behind it, or `None` for a verdict
    /// with nothing to say.
    ///
    /// The word is what [`GateObservation::clause`] needs to name the right
    /// limit ("the 256-thread **warn** ceiling"), and pairing it with the
    /// observation here is what keeps a caller from quoting one verdict's word
    /// beside another verdict's number.
    fn tripped(&self) -> Option<(&'static str, &GateObservation)> {
        match self {
            SpawnGate::Proceed => None,
            SpawnGate::Warn(observation) => Some(("warn", observation)),
            SpawnGate::Critical(observation) => Some(("critical", observation)),
        }
    }
}

/// Pure verdict over an injected free-commit reading and the machine owner's
/// floors.
///
/// `free_commit_bytes` is `Option` because that is what
/// [`crate::fleet::resource_sample::available_commit_bytes`] returns, and the
/// `None` arm is load-bearing rather than incidental: off Windows the commit
/// concept does not exist, and on Windows `GlobalMemoryStatusEx` can fail. Both
/// are UNKNOWN, and UNKNOWN produces [`SpawnGate::Proceed`] — this gate never
/// converts "I could not measure" into "you may not start".
///
/// Boundaries are **strictly below**: a machine sitting exactly on its floor is
/// at the floor, not under it. Quoting a floor of 3 GiB and then warning at
/// exactly 3 GiB would make the number the panel displays a lie by one byte.
///
/// The critical arm is tested first so an inverted configuration
/// (`critical > warn`, which the settings command refuses to persist but a
/// hand-edited `settings.json` can still contain) resolves to the heavier
/// verdict rather than silently degrading to a warning.
///
/// Testing critical first is also *why* [`coerce_ladder`] exists: it makes an
/// inverted ladder swallow the warn band whole, so the live path
/// ([`probe_for_spawn`] → [`effective_session_floors`]) clamps the pair before
/// it gets here and this function never sees an inversion in production. The arm
/// stays because `evaluate` is public to any caller with a settings struct, and
/// a pure function should be total over its inputs rather than correct only for
/// the ones its current callers happen to produce.
pub(crate) fn evaluate(
    lane: &str,
    free_commit_bytes: Option<u64>,
    guard: &SessionGuardSettings,
) -> SpawnGate {
    if !guard.enabled {
        return SpawnGate::Proceed;
    }
    let Some(free) = free_commit_bytes else {
        // Unreadable sensor ⇒ no opinion ⇒ proceed. Fail open.
        return SpawnGate::Proceed;
    };
    let observation = |limit: u64| GateObservation {
        lane: lane.to_string(),
        metric: LaneMetric::FreeCommitBytes,
        observed: free,
        limit,
    };
    if free < guard.critical_free_commit_bytes {
        return SpawnGate::Critical(observation(guard.critical_free_commit_bytes));
    }
    if free < guard.warn_free_commit_bytes {
        return SpawnGate::Warn(observation(guard.warn_free_commit_bytes));
    }
    SpawnGate::Proceed
}

/// Pure verdict over an injected thread count and the machine owner's ceilings.
/// The mirror of [`evaluate`], clause for clause.
///
/// `thread_count` is `Option` because
/// [`crate::health_monitor::thread_count_reading`] returns one, and on this lane
/// the `None` arm matters MORE than it does for free commit, not less: the
/// sensor's older `usize` form reports an unreadable snapshot as `0`, and `0`
/// compared against a ceiling is the most reassuring reading there is. UNKNOWN
/// ⇒ [`SpawnGate::Proceed`], the same fail-open posture as everywhere else in
/// this module.
///
/// Boundaries are **strictly above**, the mirror of `evaluate`'s strictly-below
/// and for the same reason: a machine sitting exactly on its ceiling is AT the
/// ceiling, not over it, and quoting a ceiling of 150 while warning at exactly
/// 150 makes the displayed number a lie by one thread.
///
/// The critical arm is tested first for the same reason as in [`evaluate`] —
/// an inverted pair (`critical < warn`, which a hand-edited `settings.json` can
/// contain) must resolve to the heavier verdict rather than silently degrade,
/// and the live path clamps the pair through [`coerce_ceiling_ladder`] before it
/// ever gets here.
pub(crate) fn evaluate_threads(
    thread_count: Option<usize>,
    guard: &SessionGuardSettings,
) -> SpawnGate {
    if !guard.enabled {
        return SpawnGate::Proceed;
    }
    let Some(threads) = thread_count else {
        // Unreadable sensor ⇒ no opinion ⇒ proceed. Fail open.
        return SpawnGate::Proceed;
    };
    let observation = |limit: usize| GateObservation {
        lane: Lane::Threads.as_str().to_string(),
        metric: LaneMetric::ThreadCount,
        observed: threads as u64,
        limit: limit as u64,
    };
    if threads > guard.critical_thread_count {
        return SpawnGate::Critical(observation(guard.critical_thread_count));
    }
    if threads > guard.warn_thread_count {
        return SpawnGate::Warn(observation(guard.warn_thread_count));
    }
    SpawnGate::Proceed
}

/// The floors actually enforced: `max(local override, cached fleet default,
/// hardcoded default)`, per field. PURE over the two injected terms.
///
/// ## Why a max, and only a max
///
/// The three terms are not a precedence chain where the most specific wins —
/// they are three parties who may each raise the bar and none of whom may lower
/// it. A tenant admin setting a fleet-wide floor is protecting fleet machines
/// they do not sit at; a machine owner who needs MORE headroom (a box that also
/// hosts a WSL build runner, say) must be able to say so; and the hardcoded
/// default is the floor below which nobody, local or remote, gets to take this
/// machine's session protection away. `max` is the only fold that gives all
/// three of those at once.
///
/// An UNKNOWN fleet term contributes **nothing** and the floor falls back to
/// `max(local, hardcoded)` — see [`tighten`]. That is the poller's fail-safe
/// contract read through to its consumer: before the first successful poll, and
/// after a 401/404, there is no fleet term at all.
///
/// ## …but a `max` alone can compose something nobody wrote
///
/// Three independent authors folded field-by-field can produce a pair neither of
/// them stated, so the fold is bounded on both ends:
/// [`tighten`] caps each floor at [`SESSION_FLOOR_MAX_BYTES`] (an unreachable
/// floor refuses every unattended spawn forever — this lane has no timeout to
/// fail open through), and [`coerce_ladder`] then forces `critical <= warn` (a
/// fleet row that states only the critical column would otherwise delete the
/// warn band entirely). Both clamps are the weakest correction that restores the
/// invariant, and both are reported: the cap through the number the panel
/// renders, the coercion through the `Option<`[`LadderCoercion`]`>` this
/// function's [`merge_floors_reporting`] form returns.
///
/// ## This fold authors the BYTE floors and nothing else
///
/// The two thread ceilings ride through untouched, exactly as `enabled` does —
/// they belong to a different lane with a different fleet key, folded by
/// [`merge_thread_ceilings`]. Reading `warn_thread_count` off this result would
/// be reading the local value, unfolded.
///
/// ## `enabled` is NOT part of the max
///
/// The master switch stays the machine owner's, and is copied through
/// untouched. The non-loosening rule is a statement about *floors*, and coord
/// publishes floors — four byte columns, no enable flag. Synthesising "the
/// fleet turns your guard back on" out of a byte value would be inventing an
/// opinion coord never expressed, which is the same reasoning
/// [`crate::ci_node::admission::defer_commit_floor_gb`] gives for treating a
/// disabled guard as `None` rather than as the floor it happens to have stored.
pub(crate) fn merge_floors(
    local: &SessionGuardSettings,
    fleet: SessionFloors,
) -> SessionGuardSettings {
    merge_floors_reporting(local, fleet).0
}

/// [`merge_floors`], plus the ladder coercion it had to apply — still PURE.
///
/// The coercion is REPORTED rather than logged in here so this function keeps
/// the property its whole design rests on: it probes nothing, reads no globals
/// and emits nothing, so every threshold argument is settleable in a test. The
/// one impure seam ([`effective_session_floors`]) decides what to do with the
/// report, exactly as `ci_node::admission` keeps `headroom_defers` pure and does
/// the reading in `probe_headroom`.
pub(crate) fn merge_floors_reporting(
    local: &SessionGuardSettings,
    fleet: SessionFloors,
) -> (SessionGuardSettings, Option<LadderCoercion>) {
    let hardcoded = SessionGuardSettings::default();
    let warn = tighten(
        local.warn_free_commit_bytes,
        hardcoded.warn_free_commit_bytes,
        fleet.warn_free_bytes,
    );
    let requested_critical = tighten(
        local.critical_free_commit_bytes,
        hardcoded.critical_free_commit_bytes,
        fleet.critical_free_bytes,
    );
    let (critical, coercion) = coerce_ladder(warn, requested_critical);
    (
        SessionGuardSettings {
            warn_free_commit_bytes: warn,
            critical_free_commit_bytes: critical,
            ..local.clone()
        },
        coercion,
    )
}

/// The thread ceilings actually enforced: `min(local override, cached fleet
/// default, hardcoded default)`, per field. PURE — the mirror of
/// [`merge_floors`].
///
/// `min` rather than `max` for exactly the reason [`merge_floors`] gives for its
/// `max`: three parties may each TIGHTEN the guard and none may loosen it, and
/// on a ceiling lane tightening means lowering. The hardcoded default is
/// therefore the LOOSEST ceiling anyone can have, not the strictest — a
/// hand-edited `settings.json` naming a 100000-thread ceiling gets 400.
///
/// The same two clamps apply, in their mirrored forms: [`tighten_ceiling`]
/// bounds each ceiling below at [`THREAD_CEILING_MIN`], and
/// [`coerce_ceiling_ladder`] then forces `critical >= warn`.
///
/// Authors the two thread fields and nothing else — the byte floors ride
/// through untouched, the mirror of the note on [`merge_floors`].
pub(crate) fn merge_thread_ceilings(
    local: &SessionGuardSettings,
    fleet: SessionFloors,
) -> SessionGuardSettings {
    merge_thread_ceilings_reporting(local, fleet).0
}

/// [`merge_thread_ceilings`], plus the ladder coercion it had to apply — still
/// PURE. Same split, same reason, as [`merge_floors_reporting`].
pub(crate) fn merge_thread_ceilings_reporting(
    local: &SessionGuardSettings,
    fleet: SessionFloors,
) -> (SessionGuardSettings, Option<LadderCoercion>) {
    let hardcoded = SessionGuardSettings::default();
    let warn = tighten_ceiling(
        local.warn_thread_count,
        hardcoded.warn_thread_count,
        fleet.warn_thread_count.map(|n| n as usize),
    );
    let requested_critical = tighten_ceiling(
        local.critical_thread_count,
        hardcoded.critical_thread_count,
        fleet.critical_thread_count.map(|n| n as usize),
    );
    let (critical, coercion) = coerce_ceiling_ladder(warn, requested_critical);
    (
        SessionGuardSettings {
            warn_thread_count: warn,
            critical_thread_count: critical,
            ..local.clone()
        },
        coercion,
    )
}

/// A ladder the three-term fold inverted, and what it was clamped to. Reported
/// by [`merge_floors_reporting`] / [`merge_thread_ceilings_reporting`], logged
/// (once, on a transition) by [`note_ladder_coercion`].
///
/// One type for both lanes rather than a sibling per lane, because there is
/// exactly one thing being reported — "the fold produced a critical limit on the
/// wrong side of the warn limit, and here is the weakest correction" — and
/// exactly one edge-triggered logging discipline that must handle it. The
/// [`LaneMetric`] is what makes the numbers renderable in the right unit and the
/// message phrasable in the right direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LadderCoercion {
    /// Which lane's limits these are, and therefore how to render them.
    pub(crate) metric: LaneMetric,
    /// The critical limit the fold produced, before clamping.
    pub(crate) requested_critical: u64,
    /// The warn limit it was clamped to.
    pub(crate) warn: u64,
}

/// Force `critical <= warn`, returning the clamped critical floor and the
/// coercion if one was needed. PURE.
///
/// ## Why the fold can invert the ladder at all
///
/// The two floors are folded independently, and they have three independent
/// authors. A tenant that sets `min_free_bytes_sessions_critical_host = 6 GiB`
/// and leaves the warn column NULL states one number; the merge then reads the
/// other from the hardcoded default (3 GiB) and produces a ladder — warn 3,
/// critical 6 — that nobody wrote. [`evaluate`] tests critical first, so on that
/// machine every reading under 6 GiB becomes a REFUSAL and the warn band ceases
/// to exist, on all eight unattended seams at once. That is precisely the state
/// `commands::resource_guard_settings::save_session_guard_settings` refuses to
/// persist locally ("a machine would block a spawn it had never warned about"),
/// so the remote path must not be able to synthesise it.
///
/// ## Why clamp critical DOWN, and never raise warn UP
///
/// Raising warn to meet critical would also restore the ordering, and it would
/// be the wrong repair: it enforces a warn floor of 6 GiB that neither the
/// tenant nor the machine owner asked for, silently tightening past both inputs
/// on the strength of an arithmetic accident. Clamping critical down keeps the
/// heaviest verdict no heavier than the lightest one's floor, which is the
/// weakest correction that restores the invariant. The tenant's intent is not
/// discarded either — their 6 GiB still raises the *warn* floor whenever they
/// state the warn column, which is the column that means "warn at 6 GiB".
///
/// Equal floors are the fixed point and are legal, exactly as the local writer's
/// `session_floors_are_inverted` treats them: "warn and block at the same point"
/// is blunt, but it is a coherent thing to mean.
fn coerce_ladder(warn: u64, critical: u64) -> (u64, Option<LadderCoercion>) {
    if critical <= warn {
        return (critical, None);
    }
    (
        warn,
        Some(LadderCoercion {
            metric: LaneMetric::FreeCommitBytes,
            requested_critical: critical,
            warn,
        }),
    )
}

/// Force `critical >= warn`, returning the clamped critical ceiling and the
/// coercion if one was needed. PURE — the mirror of [`coerce_ladder`].
///
/// ## Why the fold can invert this ladder too
///
/// Same mechanism, mirrored. The two ceilings fold independently from three
/// authors, so a fleet row (or a local edit) that states ONLY the critical
/// ceiling — `max_threads_sessions_critical = 120`, say, after a wedge — leaves
/// the warn ceiling on the hardcoded 256 and composes a ladder nobody wrote:
/// warn 256, critical 200 (the fleet's 120 having first been clamped up to
/// [`THREAD_CEILING_MIN`]). [`evaluate_threads`] tests critical first, so every
/// reading above 200 becomes a REFUSAL and the warn band between 200 and 256
/// ceases to exist, on eight unattended seams at once.
///
/// ## Why raise critical UP to warn, rather than lower warn down to it
///
/// The mirror of [`coerce_ladder`]'s argument, and it lands the same way.
/// Lowering warn to 200 would also restore the ordering, and it would enforce a
/// warn ceiling that neither party stated, tightening past both inputs on an
/// arithmetic accident — on this lane it would additionally push the warn
/// ceiling toward a count the runner can reach at rest (measured 150-151). Raising critical to the warn ceiling
/// keeps the heaviest verdict no lighter than the lightest one's limit, which is
/// the weakest correction that restores the invariant, and it preserves the
/// stated intent where it is expressible: whoever wants to refuse at 120 can say
/// so in the warn column, which is the column that means "have an opinion at
/// 120".
///
/// Equal ceilings are the fixed point and are legal, exactly as equal floors
/// are.
fn coerce_ceiling_ladder(warn: usize, critical: usize) -> (usize, Option<LadderCoercion>) {
    if critical >= warn {
        return (critical, None);
    }
    (
        warn,
        Some(LadderCoercion {
            metric: LaneMetric::ThreadCount,
            requested_critical: critical as u64,
            warn: warn as u64,
        }),
    )
}

/// `max` over the two floors that always exist and the one that may not, capped
/// at [`SESSION_FLOOR_MAX_BYTES`].
///
/// The `None` arm is spelled out rather than folded in as `unwrap_or(0)`, which
/// would be the same arithmetic and the wrong statement: an absent fleet floor
/// is UNKNOWN, and a future edit that starts treating UNKNOWN as a value has to
/// delete this arm to do it. A zero floor disables the guard it names, so the
/// distinction is worth a branch.
///
/// The cap is applied AFTER the `max`, not to each term before it, so no source
/// can escape it: a local override is bounded by exactly the same ceiling as a
/// fleet column, and neither can walk this machine past the point where it can
/// no longer start a session. See [`SESSION_FLOOR_MAX_BYTES`] for why an
/// unreachable floor is the worse failure on this lane.
fn tighten(local: u64, hardcoded: u64, fleet: Option<u64>) -> u64 {
    let known = local.max(hardcoded);
    let raised = match fleet {
        Some(fleet_floor) => known.max(fleet_floor),
        None => known,
    };
    raised.min(SESSION_FLOOR_MAX_BYTES)
}

/// `min` over the two ceilings that always exist and the one that may not,
/// clamped UP at [`THREAD_CEILING_MIN`]. The mirror of [`tighten`].
///
/// The `None` arm is spelled out for the identical reason, and the reason is if
/// anything stronger here: folded as `unwrap_or(0)` an absent fleet ceiling
/// would become a ceiling of ZERO, which on a `min` lane wins outright and
/// refuses every spawn on the machine. UNKNOWN contributes nothing, and a future
/// edit that wants to treat it as a value has to delete this arm to do it.
///
/// The clamp is applied AFTER the `min`, not to each term before it, so no
/// source can escape it — a local override is bounded by exactly the same floor
/// as a fleet column. See [`THREAD_CEILING_MIN`] for why a ceiling below the
/// runner's own at-rest thread count is a machine that can never spawn again
/// rather than a stricter guard.
fn tighten_ceiling(local: usize, hardcoded: usize, fleet: Option<usize>) -> usize {
    let known = local.min(hardcoded);
    let lowered = match fleet {
        Some(fleet_ceiling) => known.min(fleet_ceiling),
        None => known,
    };
    lowered.max(THREAD_CEILING_MIN)
}

/// The effective floors for `lane`: [`merge_floors`] over the caller's local
/// settings and the fleet's cached floors for that lane.
///
/// This is the IMPURE seam — the one place the process-global fleet cache is
/// read — exactly as `probe_headroom` is the only settings reader in
/// `ci_node/admission.rs`. It does no I/O: the read is a lock on a cache the
/// poller fills in the background, so it is safe on the spawn path.
///
/// The lane is passed in rather than assumed because the floors are
/// lane-separated and must never be crossed: a host-lane free-commit reading is
/// judged against the host floor or against nothing.
pub(crate) fn effective_session_floors(
    local: &SessionGuardSettings,
    lane: &str,
) -> SessionGuardSettings {
    let (floors, coercion) = merge_floors_reporting(
        local,
        crate::mcp::fleet_policy_poller::fleet_session_floors(lane),
    );
    note_ladder_coercion(lane, coercion);
    floors
}

/// The effective thread ceilings: [`merge_thread_ceilings`] over the caller's
/// local settings and the fleet's cached ceilings. The mirror of
/// [`effective_session_floors`], and the same single impure seam.
///
/// Takes no lane, because there is only one: the thread count is a property of
/// **this process**, not of a host/WSL resource pool, so it has exactly one set
/// of limits and looks them up under [`Lane::Threads`] — the shared lane
/// vocabulary rather than a `"threads"` literal, so a rename is a compile error
/// instead of a silently empty lookup.
///
/// That lookup returns nothing today and is expected to: **coord publishes no
/// thread column**, so the fleet term is dormant and the fold degrades to
/// `min(local, hardcoded)`, which is exactly the poller's documented fail-safe
/// for a term it has never received.
pub(crate) fn effective_thread_ceilings(local: &SessionGuardSettings) -> SessionGuardSettings {
    let lane = Lane::Threads.as_str();
    let (ceilings, coercion) = merge_thread_ceilings_reporting(
        local,
        crate::mcp::fleet_policy_poller::fleet_session_floors(lane),
    );
    note_ladder_coercion(lane, coercion);
    ceilings
}

/// The ladder coercion currently in force PER LANE, so the line is emitted on a
/// TRANSITION and not on every spawn.
///
/// A map rather than the single slot this started as, because a spawn now folds
/// two lanes and both can be coerced at once. With one slot, a host-floor
/// inversion and a thread-ceiling inversion would each see the other's state as
/// "changed" and the pair would log on every single spawn — the exact flood the
/// edge trigger exists to prevent, arriving through the mechanism meant to
/// prevent it. An absent key means "this lane is not currently coerced".
static LAST_COERCION: Mutex<BTreeMap<String, LadderCoercion>> = Mutex::new(BTreeMap::new());

/// Log a [`LadderCoercion`] once, edge-triggered.
///
/// [`effective_session_floors`] and [`effective_thread_ceilings`] run on every
/// spawn and on every `ci_node` headroom probe, so an unconditional line would
/// put the same warning in the log dozens of times an hour while telling the
/// operator nothing new. This is the same discipline
/// `mcp::fleet_policy_poller`'s loop uses for its own degradations: remember the
/// last state logged, emit only when it changes. A coercion that STOPS (the
/// tenant fixed the row, or the operator raised their warn floor) clears the
/// lane's entry, so if it ever comes back it is reported again rather than
/// swallowed as "already said that".
///
/// The lane is the map key because the limits are lane-separated: a host-lane
/// inversion, a WSL-lane inversion and a thread-lane inversion are three
/// different misconfigurations and each deserves its own line.
///
/// ONE logging discipline for both directions of inversion — the message is
/// phrased from the [`LaneMetric`] rather than duplicated per lane, because two
/// edge-triggered loggers is how one of them ends up not being edge-triggered.
fn note_ladder_coercion(lane: &str, coercion: Option<LadderCoercion>) {
    let mut last = LAST_COERCION.lock().unwrap_or_else(|e| e.into_inner());
    let changed = match coercion {
        Some(c) => last.insert(lane.to_string(), c) != Some(c),
        None => last.remove(lane).is_some(),
    };
    if !changed {
        return;
    }
    let Some(c) = coercion else {
        return;
    };
    let requested = c.metric.quantity(c.requested_critical);
    let warn = c.metric.quantity(c.warn);
    match c.metric {
        LaneMetric::FreeCommitBytes => warn!(
            lane = %lane,
            requested_critical = c.requested_critical,
            warn = c.warn,
            "resource_guard: the effective {lane} critical floor ({requested}) was above the warn \
             floor ({warn}) — clamping it to the warn floor. Left as folded, every reading below \
             the critical floor would be a refusal and nothing would ever warn. Check the tenant's \
             fleet-policy row: setting only the critical column leaves the warn column on the \
             hardcoded default."
        ),
        LaneMetric::ThreadCount => warn!(
            lane = %lane,
            requested_critical = c.requested_critical,
            warn = c.warn,
            "resource_guard: the effective {lane} critical ceiling ({requested}) was BELOW the warn \
             ceiling ({warn}) — raising it to the warn ceiling. Left as folded, every reading above \
             the critical ceiling would be a refusal and nothing would ever warn. Check the \
             tenant's fleet-policy row: setting only the critical ceiling leaves the warn ceiling \
             on the hardcoded default."
        ),
    }
}

/// Compose the two lanes' verdicts into the one this spawn is judged by, plus
/// the one that was NOT reported. PURE, which is the whole reason it is a
/// separate function: the tie-break is a policy decision and has to be arguable
/// in a test rather than only in production.
///
/// **Heavier wins.** Anything else lets a lane that measured a refusal be talked
/// out of it by a lane that measured nothing.
///
/// **On equal severity the MEMORY lane is reported.** It is the older signal, it
/// has been calibrated against a real incident's numbers since 2026-08-07, and
/// its floors are the ones the Settings panel renders and the fleet publishes —
/// so when both lanes say the same thing, the memory lane's message is the one
/// an operator can act on with the least guessing. The tie-break is a choice
/// about *which message to show*, never about which verdict applies: the verdict
/// is identical by construction on the tie.
///
/// **The unreported trip is returned, not dropped.** [`probe_for_spawn`] logs
/// it. An operator told "low memory" while the thread ceiling also tripped would
/// go free memory and watch it happen again; a guard with two sensors owes them
/// both, and a report that silently keeps one is worse than a guard with one
/// sensor because it looks complete.
fn compose_lanes(memory: SpawnGate, threads: SpawnGate) -> (SpawnGate, Option<SpawnGate>) {
    let shadowed = |other: SpawnGate| match other {
        SpawnGate::Proceed => None,
        tripped => Some(tripped),
    };
    if threads.severity() > memory.severity() {
        (threads, shadowed(memory))
    } else {
        (memory, shadowed(threads))
    }
}

/// The thread lane's live verdict, folded and evaluated. Shared by
/// [`probe_for_spawn`] and [`thread_pressure`] so the two can never drift.
fn thread_lane_verdict(local: &SessionGuardSettings) -> SpawnGate {
    let ceilings = effective_thread_ceilings(local);
    evaluate_threads(crate::health_monitor::thread_count_reading(), &ceilings)
}

/// The thread lane's verdict on its own, live — **the entry point for callers
/// that are not a spawn.**
///
/// Phase 1 of `2026-08-30-load-aware-spawn-admission-control` calls this from
/// `agent_runtime::evaluate_continuation_guard`, and it needs a DIFFERENT
/// threshold from the one [`admit_spawn`] enforces. The asymmetry is deliberate
/// and belongs to the caller, which is why this returns the whole
/// [`SpawnGate`] rather than a bool:
///
/// - A **gate continuation** may defer at [`SpawnGate::Warn`]. It can wait and
///   be re-delivered, nobody is sitting in front of it, and back-pressure that
///   arrives early is the entire point of a queue — so the cheap verdict is the
///   right one to act on.
/// - An **operator's own spawn** is refused only at [`SpawnGate::Critical`], and
///   even then overridably ([`admit_spawn`]). Refusing a human's terminal on a
///   soft signal is the false positive this module's doctrine ranks worst.
///
/// So: match on the verdict, act at the severity your caller's cost of waiting
/// justifies. Do not invent a second set of thresholds — the numbers are folded
/// once, from settings and the fleet, by [`effective_thread_ceilings`].
///
/// Short-circuits on a disabled guard before touching the sensor, exactly as
/// [`probe_for_spawn`] does: a machine owner who turned the guard off pays
/// nothing.
pub(crate) fn thread_pressure() -> SpawnGate {
    let local = crate::settings::get_session_guard_settings();
    if !local.enabled {
        return SpawnGate::Proceed;
    }
    thread_lane_verdict(&local)
}

/// Live verdict: read the limits, take one reading per lane, evaluate both,
/// report the heavier.
///
/// The settings read happens FIRST and short-circuits when the guard is
/// disabled, so a machine owner who turned the guard off pays nothing at all —
/// not one `GlobalMemoryStatusEx` call, not one thread-table walk — on every
/// spawn. [`evaluate`] and [`evaluate_threads`] also honour `enabled` so the
/// pure functions are complete on their own; the check here is about cost, not
/// correctness.
///
/// The memory reading is [`crate::fleet::resource_sample::spawn_gate_reading`] —
/// the lane name and the free-commit figure, and nothing else. It is
/// deliberately NOT the publisher's full host-lane sample: that one enumerates
/// every volume on the box, reads settings and computes build occupancy, none of
/// which this verdict consults, and this function runs synchronously on a tokio
/// worker under every unattended spawn seam. See this module's "Host lane only"
/// section for the full argument. Both paths read free commit through the same
/// `available_commit_bytes()`, so the gate and the fleet dashboard still agree on
/// the quantity.
///
/// The thread reading is [`crate::health_monitor::thread_count_reading`], which
/// is the same in-process OS-table read the health monitor has made every 60 s
/// since it shipped — no subprocess, no WMI, no allocation beyond the walk.
///
/// The fleet terms are folded in AFTER the readings, because the lane to look
/// the memory floors up under comes from the reading itself.
pub(crate) fn probe_for_spawn() -> SpawnGate {
    let local = crate::settings::get_session_guard_settings();
    if !local.enabled {
        return SpawnGate::Proceed;
    }
    let (lane, free_commit_bytes) = crate::fleet::resource_sample::spawn_gate_reading();
    let floors = effective_session_floors(&local, lane);
    let memory = evaluate(lane, free_commit_bytes, &floors);
    let threads = thread_lane_verdict(&local);

    let (reported, shadowed) = compose_lanes(memory, threads);
    // The lane that tripped but lost the report. Logged so the operator's log
    // says both, even though the toast or the refusal can only say one. The
    // severity word comes from the shadowed verdict itself, not from the
    // reported one — the two can differ, and quoting the wrong limit's name
    // beside the right limit's number is the kind of small lie that makes an
    // operator stop trusting the line. Read through `tripped()` so a `Proceed`
    // is a `None` rather than a panic arm: this runs immediately before a PTY
    // opens, and nothing on that path may be able to unwind.
    if let Some((severity, obs)) = shadowed.as_ref().and_then(SpawnGate::tripped) {
        warn!(
            lane = %obs.lane,
            metric = obs.metric.wire_name(),
            observed = obs.observed,
            limit = obs.limit,
            severity = severity,
            "resource_guard: a second lane also tripped ({}) — the reported message names the \
             other lane",
            obs.clause(severity),
        );
    }
    reported
}

/// `bytes` as `"1.42 GiB"`. Two decimals because the shipped critical floor is
/// 1.5 GiB and rounding it to `2 GiB` in the very message that quotes it would
/// misreport the configured value.
fn format_gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB)
}

/// The refusal text a CRITICAL verdict returns, prefixed for machine matching.
///
/// Names what was measured, against what, and what to do about it, because a
/// refusal that says only "not enough resources" gives the operator nothing to
/// act on — they cannot tell whether to close a build, close a session, or raise
/// a limit that was set too low. All three parts come from the
/// [`GateObservation`], so the same sentence serves either lane.
fn critical_refusal(what: &str, observation: &GateObservation) -> String {
    format!(
        "{CRITICAL_REFUSAL_PREFIX} Not starting a new {what}: {}. {} The limits live in \
         Settings > Resource Guard.",
        observation.clause("critical"),
        observation.metric.remedy(),
    )
}

/// Apply the gate to a spawn that is about to happen.
///
/// `what` is a short noun phrase for the thing being created ("terminal
/// session", "runner instance") — it lands verbatim in the operator-facing
/// message, so it must read as an object, not as a subsystem name.
///
/// `resource_override` is the caller's explicit "I know, start it anyway". It
/// only ever affects the CRITICAL arm; nothing suppresses the WARN notice,
/// because the notice is the entire product of that arm.
///
/// `app` is `Some` wherever a webview exists to receive the notice. It is
/// `Option` because [`crate::instance_manager::InstanceManager::launch_instance`]
/// can run before the AppHandle is shared (boot-time restore); a missing handle
/// downgrades to the log line and never changes the verdict.
///
/// Returns `Err(refusal)` **only** for an un-overridden CRITICAL verdict. Every
/// other path returns `Ok(())` — including every probe failure.
///
/// **Lane-agnostic by construction.** Adding the thread lane added no logic
/// here: the verdict carries its own metric, unit and phrasing
/// ([`GateObservation`]), so this function composes the same three sentences it
/// always did and they come out correct for either sensor. A second `match` on
/// which lane spoke is the thing the refactor exists to make unnecessary — and
/// the thing that would have to be kept in sync at four sites the day a third
/// sensor arrives.
pub(crate) fn admit_spawn(
    what: &str,
    resource_override: bool,
    app: Option<&AppHandle>,
) -> Result<(), String> {
    match probe_for_spawn() {
        SpawnGate::Proceed => Ok(()),
        SpawnGate::Warn(observation) => {
            let message = format!(
                "{}: {}. Starting this {what} anyway.",
                observation.metric.headline(),
                observation.clause("warn"),
            );
            warn!(
                lane = %observation.lane,
                metric = observation.metric.wire_name(),
                observed = observation.observed,
                limit = observation.limit,
                what = %what,
                "resource_guard: spawning past the session warn limit"
            );
            emit_notice(app, "warn", &observation, &message);
            Ok(())
        }
        SpawnGate::Critical(observation) => {
            if resource_override {
                let message = format!(
                    "Started this {what} even though {} — the resource guard was overridden.",
                    observation.clause("critical"),
                );
                warn!(
                    lane = %observation.lane,
                    metric = observation.metric.wire_name(),
                    observed = observation.observed,
                    limit = observation.limit,
                    what = %what,
                    "resource_guard: OVERRIDDEN — spawning past the session critical limit"
                );
                emit_notice(app, "override", &observation, &message);
                return Ok(());
            }
            warn!(
                lane = %observation.lane,
                metric = observation.metric.wire_name(),
                observed = observation.observed,
                limit = observation.limit,
                what = %what,
                "resource_guard: refusing to spawn past the session critical limit"
            );
            Err(critical_refusal(what, &observation))
        }
    }
}

/// Early-out for callers that do expensive, side-effecting work BEFORE they
/// reach the spawn seam.
///
/// [`admit_spawn`] at `TerminalSession::spawn` remains the authority — it is the
/// gate every unattended path goes through, and this one is not a replacement
/// for it. It exists because `commands::terminal::terminal_create` allocates an
/// isolated git worktree first: under `QONTINUI_AGENT_WORKTREE_MODE`,
/// `acquire_for_terminal` runs a `git worktree add`, takes a coord claim, starts
/// a heartbeat task and shells out to `git config` for the credential helper. On
/// a CRITICAL refusal all of that is thrown away, and `IsolatedEditContext::Drop`
/// releases the claim but does NOT remove the materialized worktree — so every
/// refusal leaks a directory, and the operator's "Start anyway" retry materializes
/// a second one. Refusing before the acquisition costs one
/// `GlobalMemoryStatusEx` call plus one thread-table walk, and leaks nothing.
///
/// Returns exactly what [`admit_spawn`] would: the same
/// [`CRITICAL_REFUSAL_PREFIX`]-tagged string, so the frontend's dialog and the
/// unattended callers' error handling cannot tell which of the two gates
/// answered.
///
/// **Silent on WARN, deliberately.** The warn notice is emitted once, by
/// [`admit_spawn`], at the seam the spawn actually happens on. Emitting here too
/// would put two toasts on screen for one spawn, and emitting here INSTEAD would
/// mean a caller that never reaches this pre-check gets no notice at all.
/// Deciding twice is fine — the verdict is a pure function of a reading either
/// way — but *telling the operator* twice is not.
pub(crate) fn precheck_spawn(what: &str, resource_override: bool) -> Result<(), String> {
    if resource_override {
        return Ok(());
    }
    match probe_for_spawn() {
        SpawnGate::Critical(observation) => {
            warn!(
                lane = %observation.lane,
                metric = observation.metric.wire_name(),
                observed = observation.observed,
                limit = observation.limit,
                what = %what,
                "resource_guard: refusing a {what} before its worktree/claim acquisition \
                 (pre-check; the spawn seam would refuse it too)"
            );
            Err(critical_refusal(what, &observation))
        }
        SpawnGate::Proceed | SpawnGate::Warn(_) => Ok(()),
    }
}

/// Best-effort webview notice. A failed emit is logged and swallowed: a toast
/// that could not be delivered must never turn into a spawn failure.
///
/// The payload is the generalised observation — `metric` names the unit so the
/// webview can never render a thread count as bytes, and `observed`/`limit` are
/// direction-neutral names because on one lane the reading is under the limit
/// and on the other it is over it. There is deliberately no `freeBytes` /
/// `floorBytes` alias: a compatibility field whose name is wrong for half the
/// events it carries is worse than a rename, and this fleet deletes over
/// deprecating.
fn emit_notice(
    app: Option<&AppHandle>,
    severity: &str,
    observation: &GateObservation,
    message: &str,
) {
    let Some(app) = app else {
        return;
    };
    if let Err(e) = app.emit(
        RESOURCE_GUARD_EVENT,
        serde_json::json!({
            "severity": severity,
            "lane": observation.lane,
            "metric": observation.metric.wire_name(),
            "observed": observation.observed,
            "limit": observation.limit,
            "message": message,
        }),
    ) {
        warn!("resource_guard: failed to emit {RESOURCE_GUARD_EVENT}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB_U64: u64 = 1024 * 1024 * 1024;

    /// The shipped defaults: 3 GiB warn, 1.5 GiB critical, 256/400 threads,
    /// enabled.
    fn defaults() -> SessionGuardSettings {
        SessionGuardSettings::default()
    }

    /// A fleet term that states only the two BYTE floors, which is all coord
    /// publishes today.
    fn fleet_bytes(warn: Option<u64>, critical: Option<u64>) -> SessionFloors {
        SessionFloors {
            warn_free_bytes: warn,
            critical_free_bytes: critical,
            ..SessionFloors::default()
        }
    }

    /// A fleet term that states only the two THREAD ceilings. Nothing coord
    /// ships today produces one — the wire fields are plumbed and dormant — so
    /// every case below is the shape this term will take on the day it wakes up.
    fn fleet_threads(warn: Option<u32>, critical: Option<u32>) -> SessionFloors {
        SessionFloors {
            warn_thread_count: warn,
            critical_thread_count: critical,
            ..SessionFloors::default()
        }
    }

    fn memory_observation(lane: &str, observed: u64, limit: u64) -> GateObservation {
        GateObservation {
            lane: lane.to_string(),
            metric: LaneMetric::FreeCommitBytes,
            observed,
            limit,
        }
    }

    fn thread_observation(observed: u64, limit: u64) -> GateObservation {
        GateObservation {
            lane: Lane::Threads.as_str().to_string(),
            metric: LaneMetric::ThreadCount,
            observed,
            limit,
        }
    }

    #[test]
    fn plenty_of_headroom_proceeds() {
        assert_eq!(
            evaluate("host", Some(32 * GIB_U64), &defaults()),
            SpawnGate::Proceed
        );
    }

    /// FAIL OPEN #1: an unreadable sensor is UNKNOWN, and UNKNOWN is not a
    /// reason to block. This is the arm that keeps the gate harmless off
    /// Windows (where free commit does not exist) and on a
    /// `GlobalMemoryStatusEx` failure.
    #[test]
    fn unreadable_sensor_proceeds() {
        assert_eq!(evaluate("host", None, &defaults()), SpawnGate::Proceed);
    }

    /// FAIL OPEN #2: a disabled guard has no opinion at ANY reading, including
    /// zero free commit. The machine owner's switch outranks the floors.
    #[test]
    fn disabled_guard_proceeds_at_every_reading() {
        let off = SessionGuardSettings {
            enabled: false,
            ..defaults()
        };
        for free in [None, Some(0), Some(GIB_U64), Some(64 * GIB_U64)] {
            assert_eq!(evaluate("host", free, &off), SpawnGate::Proceed);
        }
    }

    /// Between the two floors ⇒ warn, and the verdict carries the numbers the
    /// operator needs (which lane, how much is left, what it is being compared
    /// against). A verdict that carried only a boolean could not produce the
    /// message this gate's whole value is in.
    #[test]
    fn between_the_floors_warns_and_reports_both_numbers() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(2 * GIB_U64), &g),
            SpawnGate::Warn(memory_observation(
                "host",
                2 * GIB_U64,
                g.warn_free_commit_bytes
            ))
        );
    }

    #[test]
    fn below_the_critical_floor_is_critical() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(GIB_U64), &g),
            SpawnGate::Critical(memory_observation(
                "host",
                GIB_U64,
                g.critical_free_commit_bytes
            ))
        );
    }

    /// Boundaries are STRICTLY below. Sitting exactly on a floor is at the
    /// floor, not under it — otherwise the number the Settings panel displays
    /// and the number the gate enforces differ by one byte.
    #[test]
    fn exactly_at_a_floor_does_not_trip_it() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(g.warn_free_commit_bytes), &g),
            SpawnGate::Proceed
        );
        match evaluate("host", Some(g.critical_free_commit_bytes), &g) {
            SpawnGate::Warn(o) => assert_eq!(o.observed, g.critical_free_commit_bytes),
            other => panic!("expected Warn exactly at the critical floor, got {other:?}"),
        }
    }

    /// A hand-edited `settings.json` can transpose the floors (the
    /// `save_session_guard_settings` door refuses it, the file does not). The
    /// heavier verdict must win: degrading a transposed config to a warning
    /// would silently disable the block on the one machine whose config is
    /// already known to be wrong.
    #[test]
    fn inverted_floors_resolve_to_the_heavier_verdict() {
        let inverted = SessionGuardSettings {
            warn_free_commit_bytes: GIB_U64,
            critical_free_commit_bytes: 4 * GIB_U64,
            ..defaults()
        };
        match evaluate("host", Some(2 * GIB_U64), &inverted) {
            SpawnGate::Critical(o) => assert_eq!(o.limit, 4 * GIB_U64),
            other => panic!("expected Critical under transposed floors, got {other:?}"),
        }
    }

    /// The lane is carried through from the reading rather than hardcoded, so
    /// the message names the lane that was actually measured.
    #[test]
    fn lane_is_carried_from_the_reading() {
        match evaluate("wsl", Some(0), &defaults()) {
            SpawnGate::Critical(o) => assert_eq!(o.lane, "wsl"),
            other => panic!("expected Critical, got {other:?}"),
        }
    }

    /// An explicit override short-circuits the pre-check BEFORE it probes
    /// anything: the operator has already answered the only question this arm
    /// asks, and a retry that re-probed could refuse the very spawn they just
    /// authorised (the reading moves between the dialog and the retry).
    #[test]
    fn an_override_short_circuits_the_precheck() {
        assert!(precheck_spawn("terminal session", true).is_ok());
    }

    /// The refusal string is what the operator reads and what
    /// `src/lib/resourceGuard.ts` matches on: prefix first, then the lane, the
    /// live headroom and the configured floor.
    #[test]
    fn refusal_names_the_prefix_the_lane_the_headroom_and_the_floor() {
        let msg = critical_refusal(
            "terminal session",
            &memory_observation("host", 1_073_741_824, 1_610_612_736),
        );
        assert!(msg.starts_with(CRITICAL_REFUSAL_PREFIX));
        assert!(msg.contains("terminal session"));
        assert!(msg.contains("host lane"));
        assert!(msg.contains("1.00 GiB"), "missing headroom: {msg}");
        assert!(msg.contains("1.50 GiB"), "missing floor: {msg}");
    }

    /// 1.5 GiB must render as `1.50 GiB`, not `2 GiB` — the default critical
    /// floor has no integer-GiB spelling and the message quotes it verbatim.
    #[test]
    fn format_gib_keeps_the_fractional_default_floor_honest() {
        assert_eq!(format_gib(3 * GIB_U64 / 2), "1.50 GiB");
        assert_eq!(format_gib(3 * GIB_U64), "3.00 GiB");
    }

    // =======================================================================
    // The thread lane: same three verdicts, opposite direction
    // (plan 2026-08-30-load-aware-spawn-admission-control, Phase 2)
    // =======================================================================

    /// An idle-to-busy runner is under the warn ceiling and gets no opinion.
    /// **151 is not a made-up number**: it is what a live idle runner was
    /// measured carrying on 2026-08-30 (`/proc/<pid>/task`, sampled every 3 s).
    /// If the guard has an opinion at that reading it has an opinion on every
    /// spawn of a machine doing nothing, which is not a warning, it is noise.
    #[test]
    fn a_normal_thread_count_proceeds() {
        for threads in [1, 64, 100, 130, 151, 200] {
            assert_eq!(
                evaluate_threads(Some(threads), &defaults()),
                SpawnGate::Proceed,
                "{threads} threads is inside the at-rest band"
            );
        }
    }

    /// FAIL OPEN #1, thread lane. `None` is the reading
    /// `health_monitor::thread_count_reading` returns off Windows without
    /// procfs, and on a failed Toolhelp snapshot — which happens under exactly
    /// the memory pressure that makes this gate matter. UNKNOWN is not a reason
    /// to block.
    ///
    /// This is also the arm that makes the `Option` worth introducing: the old
    /// `usize` sensor reported the same failure as `0`, and `0 > 400` is false,
    /// so a failed snapshot would have read as a perfectly idle process.
    #[test]
    fn an_unreadable_thread_count_proceeds() {
        assert_eq!(evaluate_threads(None, &defaults()), SpawnGate::Proceed);
    }

    /// FAIL OPEN #2, thread lane. One switch covers both lanes, so a disabled
    /// guard has no opinion at any thread count — including the 540 the wedged
    /// process actually carried, and a count no machine could reach.
    #[test]
    fn disabled_guard_proceeds_at_every_thread_count() {
        let off = SessionGuardSettings {
            enabled: false,
            ..defaults()
        };
        for threads in [None, Some(0), Some(151), Some(540), Some(1_000_000)] {
            assert_eq!(evaluate_threads(threads, &off), SpawnGate::Proceed);
        }
    }

    /// Between the ceilings ⇒ warn, carrying the numbers the message quotes.
    #[test]
    fn between_the_ceilings_warns_and_reports_both_numbers() {
        let g = defaults();
        assert_eq!(
            evaluate_threads(Some(300), &g),
            SpawnGate::Warn(thread_observation(300, g.warn_thread_count as u64))
        );
    }

    /// Above the critical ceiling ⇒ critical. 540 is the count the wedged
    /// process carried on 2026-08-29.
    #[test]
    fn above_the_critical_ceiling_is_critical() {
        let g = defaults();
        assert_eq!(
            evaluate_threads(Some(540), &g),
            SpawnGate::Critical(thread_observation(540, g.critical_thread_count as u64))
        );
    }

    /// Boundaries are STRICTLY above — the mirror of the floor lane's strictly
    /// below, and for the same reason: a machine sitting exactly ON its ceiling
    /// is at the ceiling, not over it, and quoting "the 150-thread warn
    /// ceiling" while warning at exactly 150 makes the displayed number a lie
    /// by one thread.
    #[test]
    fn exactly_at_a_ceiling_does_not_trip_it() {
        let g = defaults();
        assert_eq!(
            evaluate_threads(Some(g.warn_thread_count), &g),
            SpawnGate::Proceed,
            "exactly at the warn ceiling is not past it"
        );
        match evaluate_threads(Some(g.warn_thread_count + 1), &g) {
            SpawnGate::Warn(o) => assert_eq!(o.observed, g.warn_thread_count as u64 + 1),
            other => panic!("expected Warn one thread over the warn ceiling, got {other:?}"),
        }
        match evaluate_threads(Some(g.critical_thread_count), &g) {
            SpawnGate::Warn(o) => assert_eq!(o.observed, g.critical_thread_count as u64),
            other => panic!("expected Warn exactly at the critical ceiling, got {other:?}"),
        }
        match evaluate_threads(Some(g.critical_thread_count + 1), &g) {
            SpawnGate::Critical(o) => assert_eq!(o.limit, g.critical_thread_count as u64),
            other => {
                panic!("expected Critical one thread over the critical ceiling, got {other:?}")
            }
        }
    }

    /// A hand-edited `settings.json` can transpose the ceilings too, and the
    /// heavier verdict must win for the same reason it does on the floor lane.
    #[test]
    fn inverted_ceilings_resolve_to_the_heavier_verdict() {
        let inverted = SessionGuardSettings {
            warn_thread_count: 400,
            critical_thread_count: 150,
            ..defaults()
        };
        match evaluate_threads(Some(200), &inverted) {
            SpawnGate::Critical(o) => assert_eq!(o.limit, 150),
            other => panic!("expected Critical under transposed ceilings, got {other:?}"),
        }
    }

    /// The thread lane names itself through the shared lane vocabulary, never a
    /// literal — the same rule the fleet-limit cache's `for_lane` depends on.
    #[test]
    fn the_thread_lane_uses_the_shared_lane_name() {
        match evaluate_threads(Some(10_000), &defaults()) {
            SpawnGate::Critical(o) => {
                assert_eq!(o.lane, "threads");
                assert_eq!(o.lane, Lane::Threads.as_str());
            }
            other => panic!("expected Critical, got {other:?}"),
        }
    }

    // =======================================================================
    // Rendering: one template, two units, two directions
    // =======================================================================

    /// The whole point of [`GateObservation`]: the SAME message-composing code
    /// says "below the … floor" in GiB for one lane and "above the …-thread
    /// ceiling" for the other. Rendering 412 threads through `format_gib` would
    /// have produced a cheerful `0.00 GiB` and no compiler complaint.
    #[test]
    fn each_metric_renders_in_its_own_unit_and_direction() {
        let memory = memory_observation("host", 1_524_713_390, 3 * GIB_U64);
        assert_eq!(memory.observed_display(), "1.42 GiB");
        assert_eq!(memory.limit_display(), "3.00 GiB");
        assert_eq!(
            memory.clause("warn"),
            "the host lane has 1.42 GiB of free commit, below the 3.00 GiB warn floor"
        );

        let threads = thread_observation(412, 150);
        assert_eq!(threads.observed_display(), "412 threads");
        assert_eq!(threads.limit_display(), "150 threads");
        assert_eq!(
            threads.clause("warn"),
            "the runner process is carrying 412 threads, above the 150-thread warn ceiling"
        );
    }

    /// A thread-lane refusal is still a machine-recognisable refusal — the
    /// prefix `src/lib/resourceGuard.ts` matches on is a stable contract across
    /// both lanes — and its remedy tells the operator to wait for sessions, not
    /// to free memory they already have plenty of.
    #[test]
    fn a_thread_refusal_keeps_the_prefix_and_names_the_right_remedy() {
        let msg = critical_refusal("terminal session", &thread_observation(540, 400));
        assert!(msg.starts_with(CRITICAL_REFUSAL_PREFIX));
        assert!(msg.contains("540 threads"), "missing reading: {msg}");
        assert!(
            msg.contains("400-thread critical ceiling"),
            "missing limit: {msg}"
        );
        assert!(
            msg.contains("sessions finish"),
            "a thread refusal must not tell the operator to free memory: {msg}"
        );
        assert!(!msg.contains("GiB"), "no byte unit belongs here: {msg}");
    }

    // =======================================================================
    // The three-term effective floor (plan Part B: max(local, fleet, hardcoded))
    // =======================================================================

    /// No fleet term at all — before the first poll, after a 401/404, on an
    /// unpaired runner, or on a coord that predates the columns. The floors must
    /// be EXACTLY what they were before the poller existed. This is the arm that
    /// runs today, and the one that has to keep running when coord is
    /// unreachable, which is when this gate matters most.
    #[test]
    fn an_absent_fleet_term_changes_nothing() {
        let local = defaults();
        assert_eq!(merge_floors(&local, SessionFloors::default()), local);

        // …including for an owner who tightened locally: their own floors
        // survive an empty cache untouched.
        let tightened = SessionGuardSettings {
            warn_free_commit_bytes: 8 * GIB_U64,
            critical_free_commit_bytes: 4 * GIB_U64,
            ..defaults()
        };
        assert_eq!(
            merge_floors(&tightened, SessionFloors::default()),
            tightened
        );
    }

    /// A fleet floor ABOVE the local one wins: the tenant may tighten a machine
    /// it does not sit at.
    #[test]
    fn a_higher_fleet_floor_tightens_the_local_one() {
        let merged = merge_floors(
            &defaults(),
            fleet_bytes(Some(6 * GIB_U64), Some(3 * GIB_U64)),
        );
        assert_eq!(merged.warn_free_commit_bytes, 6 * GIB_U64);
        assert_eq!(merged.critical_free_commit_bytes, 3 * GIB_U64);
    }

    /// A fleet floor BELOW the local one loses. The fleet default is a default,
    /// not a ceiling — it can never talk a machine owner down out of protection
    /// they asked for.
    #[test]
    fn a_lower_fleet_floor_never_loosens_a_local_one() {
        let tightened = SessionGuardSettings {
            warn_free_commit_bytes: 10 * GIB_U64,
            critical_free_commit_bytes: 5 * GIB_U64,
            ..defaults()
        };
        let merged = merge_floors(&tightened, fleet_bytes(Some(4 * GIB_U64), Some(GIB_U64)));
        assert_eq!(merged.warn_free_commit_bytes, 10 * GIB_U64);
        assert_eq!(merged.critical_free_commit_bytes, 5 * GIB_U64);
    }

    /// The hardcoded default is the last line: neither a low local value nor a
    /// low fleet value can take this machine's protection below it. A
    /// hand-edited `settings.json` naming a 1 MiB warn floor is the case that
    /// matters — that file is not validated on read.
    #[test]
    fn neither_local_nor_fleet_can_go_below_the_hardcoded_default() {
        let loosened = SessionGuardSettings {
            warn_free_commit_bytes: 1024 * 1024,
            critical_free_commit_bytes: 1,
            ..defaults()
        };
        let hardcoded = SessionGuardSettings::default();

        let merged = merge_floors(&loosened, SessionFloors::default());
        assert_eq!(
            merged.warn_free_commit_bytes,
            hardcoded.warn_free_commit_bytes
        );
        assert_eq!(
            merged.critical_free_commit_bytes,
            hardcoded.critical_free_commit_bytes
        );

        // A fleet ZERO is the same story: the fleet is entitled to say zero, and
        // saying it cannot disable the guard, because the hardcoded default is
        // still a term in the max.
        let with_fleet_zero = merge_floors(&loosened, fleet_bytes(Some(0), Some(0)));
        assert_eq!(
            with_fleet_zero.warn_free_commit_bytes,
            hardcoded.warn_free_commit_bytes
        );
        assert_eq!(
            with_fleet_zero.critical_free_commit_bytes,
            hardcoded.critical_free_commit_bytes
        );
    }

    /// The two floors fold independently AS FAR AS THE LADDER ALLOWS: a fleet
    /// that states only the warn floor must not drag the critical floor with it
    /// in either direction — but the fold is not free to invert the ladder,
    /// which is what the coercion tests below pin. Here the ordering survives
    /// the fold (9 GiB warn is above the 1.5 GiB critical default), so nothing
    /// is coerced and independence is visible in the result.
    #[test]
    fn the_two_floors_fold_independently_while_the_ladder_holds() {
        let (merged, coercion) =
            merge_floors_reporting(&defaults(), fleet_bytes(Some(9 * GIB_U64), None));
        assert_eq!(merged.warn_free_commit_bytes, 9 * GIB_U64);
        assert_eq!(
            merged.critical_free_commit_bytes,
            defaults().critical_free_commit_bytes
        );
        assert_eq!(coercion, None, "an ordered fold coerces nothing");
    }

    // =======================================================================
    // The ladder invariant: `critical <= warn`, whatever the three terms say
    // =======================================================================

    /// THE CASE THIS EXISTS FOR. A tenant sets ONLY the critical column — a
    /// perfectly ordinary thing to do after an incident — and leaves the warn
    /// column NULL. Folded independently that yields warn 3 GiB (hardcoded) and
    /// critical 6 GiB (fleet), and since `evaluate` tests critical first, every
    /// reading under 6 GiB becomes a REFUSAL on every unattended seam of every
    /// machine in the tenant, with no warn band left at all. The merge must
    /// clamp it instead.
    #[test]
    fn a_fleet_critical_floor_with_a_null_warn_column_cannot_invert_the_ladder() {
        let (merged, coercion) =
            merge_floors_reporting(&defaults(), fleet_bytes(None, Some(6 * GIB_U64)));

        // The warn floor is NOT raised to meet the critical one: that would
        // enforce 6 GiB of warn nobody asked for.
        assert_eq!(
            merged.warn_free_commit_bytes,
            defaults().warn_free_commit_bytes
        );
        // The critical floor is clamped down to it.
        assert_eq!(
            merged.critical_free_commit_bytes,
            merged.warn_free_commit_bytes
        );
        assert_eq!(
            coercion,
            Some(LadderCoercion {
                metric: LaneMetric::FreeCommitBytes,
                requested_critical: 6 * GIB_U64,
                warn: 3 * GIB_U64,
            })
        );

        // And the verdict that would have been a refusal is a refusal only
        // below the warn floor now — 4 GiB proceeds instead of being blocked.
        assert_eq!(
            evaluate("host", Some(4 * GIB_U64), &merged),
            SpawnGate::Proceed
        );
    }

    /// The invariant holds however the inversion is assembled — from a local
    /// override, from the fleet, or from the two of them crossing.
    #[test]
    fn every_source_of_an_inversion_is_coerced() {
        // Local only: a hand-edited `settings.json` (the save command refuses
        // this, the file does not).
        let local_inverted = SessionGuardSettings {
            warn_free_commit_bytes: 4 * GIB_U64,
            critical_free_commit_bytes: 9 * GIB_U64,
            ..defaults()
        };
        let merged = merge_floors(&local_inverted, SessionFloors::default());
        assert_eq!(merged.warn_free_commit_bytes, 4 * GIB_U64);
        assert_eq!(merged.critical_free_commit_bytes, 4 * GIB_U64);

        // Crossed terms: the machine owner states the warn floor, the tenant
        // states a critical floor above it. Neither party wrote an inverted
        // ladder; the `max` composed one.
        let merged = merge_floors(
            &SessionGuardSettings {
                warn_free_commit_bytes: 5 * GIB_U64,
                critical_free_commit_bytes: 2 * GIB_U64,
                ..defaults()
            },
            fleet_bytes(None, Some(7 * GIB_U64)),
        );
        assert_eq!(merged.warn_free_commit_bytes, 5 * GIB_U64);
        assert_eq!(merged.critical_free_commit_bytes, 5 * GIB_U64);
    }

    /// Equal floors are the fixed point, not an inversion — the same rule the
    /// local writer's `session_floors_are_inverted` applies. Coercing them
    /// would report a misconfiguration on every spawn of a machine that has
    /// none.
    #[test]
    fn equal_floors_are_not_a_coercion() {
        let equal = SessionGuardSettings {
            warn_free_commit_bytes: 5 * GIB_U64,
            critical_free_commit_bytes: 5 * GIB_U64,
            ..defaults()
        };
        let (merged, coercion) = merge_floors_reporting(&equal, SessionFloors::default());
        assert_eq!(merged, equal);
        assert_eq!(coercion, None);
        assert_eq!(coerce_ladder(5, 5), (5, None));
    }

    /// The merged floors always satisfy the invariant `evaluate` depends on,
    /// across the whole cross-product of plausible terms. `evaluate` tests
    /// critical first, so this is the property that keeps a warn band from
    /// silently disappearing.
    #[test]
    fn the_merged_ladder_is_always_ordered() {
        let locals = [0, GIB_U64 / 2, GIB_U64, 3 * GIB_U64, 9 * GIB_U64, u64::MAX];
        let fleets = [None, Some(0), Some(6 * GIB_U64), Some(u64::MAX)];
        for lw in locals {
            for lc in locals {
                for fw in fleets {
                    for fc in fleets {
                        let merged = merge_floors(
                            &SessionGuardSettings {
                                warn_free_commit_bytes: lw,
                                critical_free_commit_bytes: lc,
                                ..defaults()
                            },
                            fleet_bytes(fw, fc),
                        );
                        assert!(
                            merged.critical_free_commit_bytes <= merged.warn_free_commit_bytes,
                            "inverted: local({lw},{lc}) fleet({fw:?},{fc:?}) {merged:?}"
                        );
                        assert!(
                            merged.warn_free_commit_bytes <= SESSION_FLOOR_MAX_BYTES,
                            "uncapped: local({lw},{lc}) fleet({fw:?},{fc:?})"
                        );
                    }
                }
            }
        }
    }

    // =======================================================================
    // The upper clamp (this lane fails CLOSED — an unreachable floor is fatal)
    // =======================================================================

    /// A fleet column is a `BIGINT` whose only validation is its sign, so
    /// `i64::MAX` reaches the fold intact. Uncapped it would make every spawn on
    /// every machine in the tenant refuse forever, with no timeout to fail open
    /// through and no override on eight of the ten seams.
    #[test]
    fn an_absurd_fleet_floor_is_capped_not_honoured() {
        let merged = merge_floors(&defaults(), fleet_bytes(Some(u64::MAX), Some(u64::MAX)));
        assert_eq!(merged.warn_free_commit_bytes, SESSION_FLOOR_MAX_BYTES);
        assert_eq!(merged.critical_free_commit_bytes, SESSION_FLOOR_MAX_BYTES);
    }

    /// The cap is applied AFTER the max, so a LOCAL override cannot escape it
    /// either — the panel accepts up to 128 GiB and `settings.json` accepts any
    /// `u64`, neither of which is a reachable floor on a 32 GB box.
    #[test]
    fn a_local_override_cannot_escape_the_cap() {
        let absurd = SessionGuardSettings {
            warn_free_commit_bytes: 128 * GIB_U64,
            critical_free_commit_bytes: 64 * GIB_U64,
            ..defaults()
        };
        let merged = merge_floors(&absurd, SessionFloors::default());
        assert_eq!(merged.warn_free_commit_bytes, SESSION_FLOOR_MAX_BYTES);
        assert_eq!(merged.critical_free_commit_bytes, SESSION_FLOOR_MAX_BYTES);
    }

    /// The cap bounds the ceiling and nothing else: everything under it passes
    /// through untouched, so the clamp cannot be mistaken for a second floor.
    #[test]
    fn the_cap_leaves_every_reachable_floor_alone() {
        let reachable = SessionGuardSettings {
            warn_free_commit_bytes: SESSION_FLOOR_MAX_BYTES,
            critical_free_commit_bytes: SESSION_FLOOR_MAX_BYTES - 1,
            ..defaults()
        };
        let merged = merge_floors(&reachable, SessionFloors::default());
        assert_eq!(merged, reachable);
    }

    /// Cross-lane pin. This cap also feeds `ci_node` admission, whose own
    /// `defer_commit_floor_gb` clamps at `MAX_SESSION_DEFER_FLOOR_GB`. Capping
    /// below that would make the CI lane's session term inert for every setting
    /// — `max(DEFER_FREE_COMMIT_GB, min(floor, 12))` would be a constant — so
    /// the two bounds have to be changed together, and this test is where that
    /// gets noticed.
    #[test]
    fn the_cap_matches_the_ci_lanes_own_session_floor_bound() {
        assert_eq!(
            SESSION_FLOOR_MAX_BYTES / GIB_U64,
            crate::ci_node::admission::MAX_SESSION_DEFER_FLOOR_GB
        );
        // …and it must sit above the shipped defaults, or the setting the panel
        // offers could never do anything.
        assert!(SESSION_FLOOR_MAX_BYTES > defaults().warn_free_commit_bytes);
    }

    /// The master switch is the machine owner's and is copied through: coord
    /// publishes byte floors, not an enable flag, so a fleet floor must never be
    /// read as "turn the guard back on".
    #[test]
    fn the_fleet_term_never_re_enables_a_disabled_guard() {
        let off = SessionGuardSettings {
            enabled: false,
            ..defaults()
        };
        let merged = merge_floors(&off, fleet_bytes(Some(32 * GIB_U64), Some(16 * GIB_U64)));
        assert!(!merged.enabled);
        // And the pure verdict still proceeds at every reading.
        assert_eq!(evaluate("host", Some(0), &merged), SpawnGate::Proceed);

        // Same for the thread lane, whose fleet term is likewise limits-only.
        let merged = merge_thread_ceilings(&off, fleet_threads(Some(10), Some(20)));
        assert!(!merged.enabled);
        assert_eq!(evaluate_threads(Some(9_999), &merged), SpawnGate::Proceed);
    }

    /// End to end over the pure parts: a fleet floor that the local machine is
    /// under changes the VERDICT, not just the number. This is the whole point
    /// of the term — a tenant-wide tightening has to be able to produce a
    /// warning the local settings alone would not have produced.
    #[test]
    fn a_fleet_floor_can_change_the_verdict() {
        let local = defaults();
        let reading = Some(4 * GIB_U64);

        // Local floors alone: 4 GiB is above the 3 GiB warn floor. No opinion.
        let local_only = merge_floors(&local, SessionFloors::default());
        assert_eq!(evaluate("host", reading, &local_only), SpawnGate::Proceed);

        // The tenant declares a 6 GiB warn floor; the same reading now warns.
        let fleet = fleet_bytes(Some(6 * GIB_U64), None);
        assert_eq!(
            evaluate("host", reading, &merge_floors(&local, fleet)),
            SpawnGate::Warn(memory_observation("host", 4 * GIB_U64, 6 * GIB_U64))
        );
    }

    // =======================================================================
    // The three-term effective CEILING: min(local, fleet, hardcoded), clamped up
    // =======================================================================

    /// The fold's three arms, in the one direction they are allowed to move.
    #[test]
    fn tighten_ceiling_takes_the_smallest_term_that_exists() {
        // A local override tightens (a smaller ceiling wins).
        assert_eq!(tighten_ceiling(200, 400, None), 200);
        // A looser local value loses to the hardcoded default: the hardcoded
        // number is the LOOSEST anyone may have, the mirror of it being the
        // strictest on the floor lane.
        assert_eq!(tighten_ceiling(100_000, 400, None), 400);
        // The fleet tightens past both.
        assert_eq!(tighten_ceiling(400, 400, Some(250)), 250);
        // A looser fleet term changes nothing.
        assert_eq!(tighten_ceiling(300, 400, Some(390)), 300);
    }

    /// An ABSENT fleet term contributes NOTHING — it is not a zero, and the
    /// arithmetic difference is the whole machine: on a `min` lane, `0` wins
    /// outright and would refuse every spawn on the box. This is the arm that
    /// runs today, since coord publishes no thread column at all.
    #[test]
    fn an_absent_fleet_ceiling_contributes_nothing() {
        assert_eq!(tighten_ceiling(400, 400, None), 400);
        assert_eq!(
            merge_thread_ceilings(&defaults(), SessionFloors::default()),
            defaults(),
            "the dormant fleet term must leave a default machine exactly as it was"
        );
        // …and a zero fleet ceiling, which IS a statement, is still bounded by
        // the clamp rather than taken literally.
        assert_eq!(tighten_ceiling(400, 400, Some(0)), THREAD_CEILING_MIN);
    }

    /// THE CLAMP. No combination of the three terms may compose a ceiling the
    /// runner is already over at rest — that is not a stricter guard, it is a
    /// machine that can never start a session again, on eight unattended seams
    /// with nobody to press "Start anyway".
    #[test]
    fn no_combination_of_terms_can_make_the_machine_unspawnable() {
        let ceilings = [0usize, 1, 64, 151, THREAD_CEILING_MIN, 256, 400, usize::MAX];
        let fleets = [None, Some(0), Some(1), Some(64), Some(300), Some(u32::MAX)];
        for lw in ceilings {
            for lc in ceilings {
                for fw in fleets {
                    for fc in fleets {
                        let merged = merge_thread_ceilings(
                            &SessionGuardSettings {
                                warn_thread_count: lw,
                                critical_thread_count: lc,
                                ..defaults()
                            },
                            fleet_threads(fw, fc),
                        );
                        assert!(
                            merged.critical_thread_count >= THREAD_CEILING_MIN,
                            "unspawnable: local({lw},{lc}) fleet({fw:?},{fc:?}) {merged:?}"
                        );
                        assert!(
                            merged.critical_thread_count >= merged.warn_thread_count,
                            "inverted: local({lw},{lc}) fleet({fw:?},{fc:?}) {merged:?}"
                        );
                        assert!(
                            merged.warn_thread_count <= defaults().warn_thread_count,
                            "loosened: local({lw},{lc}) fleet({fw:?},{fc:?}) {merged:?}"
                        );
                        // The MEASURED at-rest count (150-151 on 2026-08-30)
                        // still proceeds under EVERY composable configuration.
                        // This is the property the clamp exists to buy, and the
                        // reading it has to be measured against — the stale
                        // 100-130 band in `health_monitor`'s doc would have let
                        // a clamp of 150 pass this test and wedge the box.
                        assert_eq!(
                            evaluate_threads(Some(151), &merged),
                            SpawnGate::Proceed,
                            "an idle runner must never be refused: {merged:?}"
                        );
                    }
                }
            }
        }
    }

    /// Every number on this lane is anchored to something measurable, and the
    /// relationships between them are what a future edit must not break.
    #[test]
    fn the_thread_numbers_stay_in_their_measured_relationships() {
        // Strictly ABOVE the health monitor's log threshold, never equal to it:
        // a live idle runner was measured at 150-151 threads on 2026-08-30, so
        // a ceiling AT that constant fires on an idle box.
        assert!(
            THREAD_CEILING_MIN > crate::health_monitor::THREAD_WARNING_THRESHOLD,
            "a clamp at or below the at-rest band makes the machine unspawnable"
        );
        assert!(defaults().warn_thread_count > crate::health_monitor::THREAD_WARNING_THRESHOLD);

        // The clamp sits strictly below both defaults, or it pins a knob: the
        // `min` fold already forbids loosening, so a clamp equal to a default
        // would make that ceiling untunable in both directions.
        assert!(THREAD_CEILING_MIN < defaults().warn_thread_count);
        assert!(THREAD_CEILING_MIN < defaults().critical_thread_count);

        // A usable warn band, or the warn verdict could never fire.
        assert!(defaults().critical_thread_count > defaults().warn_thread_count);

        // Both ceilings are fractions of tokio's 512-slot blocking pool
        // (unreconfigured in every production runtime in this binary): half of
        // it warns, and the critical ceiling leaves ~112 slots for the race
        // between reading the count and the PTY actually opening.
        assert_eq!(defaults().warn_thread_count, 512 / 2);
        assert!(defaults().critical_thread_count <= 512 - 100);
    }

    /// THE CEILING LADDER'S OWN CASE. A tenant that has just watched a machine
    /// wedge sets ONLY the critical ceiling — 120 — and leaves the warn ceiling
    /// NULL. Two clamps fire in order, and both are needed:
    ///
    /// 1. [`tighten_ceiling`] raises 120 to [`THREAD_CEILING_MIN`] (200),
    ///    because a live runner idles at 150-151 and a 120-thread ceiling is a
    ///    machine that can never start a session again.
    /// 2. That still leaves critical 200 BELOW the warn ceiling 256, and since
    ///    [`evaluate_threads`] tests critical first, every reading over 200
    ///    would be a refusal with no warn band left at all. The ladder coercion
    ///    raises critical to the warn ceiling.
    #[test]
    fn a_fleet_critical_ceiling_with_a_null_warn_column_cannot_invert_the_ladder() {
        let (merged, coercion) =
            merge_thread_ceilings_reporting(&defaults(), fleet_threads(None, Some(120)));

        // The warn ceiling is NOT lowered to meet the critical one: that would
        // enforce a ceiling nobody stated, below the measured at-rest band,
        // warning on every spawn forever.
        assert_eq!(merged.warn_thread_count, defaults().warn_thread_count);
        // The critical ceiling is raised to it — the weakest correction.
        assert_eq!(merged.critical_thread_count, merged.warn_thread_count);
        assert_eq!(
            coercion,
            Some(LadderCoercion {
                metric: LaneMetric::ThreadCount,
                // Post-clamp: the fleet's 120 was already raised to 200 by
                // `tighten_ceiling` before the ladder saw it — the same
                // composition order the floor lane uses (clamp, then ladder).
                requested_critical: THREAD_CEILING_MIN as u64,
                warn: defaults().warn_thread_count as u64,
            })
        );

        // And the machine is still spawnable at its measured idle count.
        assert_eq!(evaluate_threads(Some(151), &merged), SpawnGate::Proceed);
    }

    /// …and the case where the clamp CANNOT hide it: a machine owner who
    /// tightened their warn ceiling to 380, plus a tenant who states a critical
    /// ceiling of 200. Neither party wrote an inverted ladder; the `min`
    /// composed one, both terms are above the floor constant, and the ladder
    /// coercion is the only thing left to fix it.
    #[test]
    fn crossed_terms_above_the_floor_constant_still_invert_and_are_coerced() {
        let owner = SessionGuardSettings {
            warn_thread_count: 220,
            ..defaults()
        };
        let (merged, coercion) =
            merge_thread_ceilings_reporting(&owner, fleet_threads(None, Some(210)));

        // The warn ceiling is NOT dragged down to 210: that would enforce a
        // limit neither party stated, tightening past both inputs.
        assert_eq!(merged.warn_thread_count, 220);
        // The critical ceiling is raised to it — the weakest correction.
        assert_eq!(merged.critical_thread_count, 220);
        assert_eq!(
            coercion,
            Some(LadderCoercion {
                metric: LaneMetric::ThreadCount,
                requested_critical: 210,
                warn: 220,
            })
        );
    }

    /// The ladder coercion is visible when the two clamps do not already hide
    /// it: a local pair that is inverted well above the floor constant.
    #[test]
    fn an_inverted_local_ceiling_pair_is_corrected_the_weak_way() {
        let inverted = SessionGuardSettings {
            warn_thread_count: 240,
            critical_thread_count: 210,
            ..defaults()
        };
        let (merged, coercion) =
            merge_thread_ceilings_reporting(&inverted, SessionFloors::default());
        assert_eq!(merged.warn_thread_count, 240, "warn is never dragged down");
        assert_eq!(
            merged.critical_thread_count, 240,
            "critical is raised to it"
        );
        assert_eq!(
            coercion,
            Some(LadderCoercion {
                metric: LaneMetric::ThreadCount,
                requested_critical: 210,
                warn: 240,
            })
        );
    }

    /// Equal ceilings are the legal fixed point, exactly as equal floors are.
    #[test]
    fn equal_ceilings_are_not_a_coercion() {
        assert_eq!(coerce_ceiling_ladder(300, 300), (300, None));
        let equal = SessionGuardSettings {
            warn_thread_count: 240,
            critical_thread_count: 240,
            ..defaults()
        };
        let (merged, coercion) = merge_thread_ceilings_reporting(&equal, SessionFloors::default());
        assert_eq!(merged, equal);
        assert_eq!(coercion, None);
    }

    /// A fleet ceiling can change the VERDICT, not just the number — the same
    /// end-to-end property the floor lane's fleet term has, on the day coord
    /// starts publishing the column.
    #[test]
    fn a_fleet_ceiling_can_change_the_verdict() {
        let local = defaults();
        let reading = Some(320);

        // Local ceilings alone: 320 is between 150 and 400, so it warns.
        let local_only = merge_thread_ceilings(&local, SessionFloors::default());
        assert!(matches!(
            evaluate_threads(reading, &local_only),
            SpawnGate::Warn(_)
        ));

        // The tenant declares a 300-thread critical ceiling; the same reading
        // is now a refusal.
        let merged = merge_thread_ceilings(&local, fleet_threads(None, Some(300)));
        assert_eq!(
            evaluate_threads(reading, &merged),
            SpawnGate::Critical(thread_observation(320, 300))
        );
    }

    /// Each fold authors ITS OWN lane's two fields and copies the other lane's
    /// through untouched. Neither result is a fully-folded settings struct, and
    /// the two lanes' terms must never leak into one another — a thread ceiling
    /// silently reset by a memory fold is a limit that stops enforcing on the
    /// spawn path with nothing logged.
    #[test]
    fn each_fold_leaves_the_other_lanes_fields_alone() {
        let local = SessionGuardSettings {
            warn_free_commit_bytes: 8 * GIB_U64,
            critical_free_commit_bytes: 4 * GIB_U64,
            warn_thread_count: 200,
            critical_thread_count: 300,
            enabled: true,
        };

        let floors = merge_floors(&local, fleet_bytes(Some(9 * GIB_U64), None));
        assert_eq!(floors.warn_free_commit_bytes, 9 * GIB_U64);
        assert_eq!(floors.warn_thread_count, 200);
        assert_eq!(floors.critical_thread_count, 300);

        let ceilings = merge_thread_ceilings(&local, fleet_threads(None, Some(250)));
        assert_eq!(ceilings.critical_thread_count, 250);
        // The owner tightened their warn ceiling to 200 and it survives: 200 is
        // below the hardcoded 256 (so the `min` keeps it) and at the clamp (so
        // the clamp leaves it alone).
        assert_eq!(ceilings.warn_thread_count, 200);
        assert_eq!(ceilings.warn_free_commit_bytes, 8 * GIB_U64);
        assert_eq!(ceilings.critical_free_commit_bytes, 4 * GIB_U64);
    }

    // =======================================================================
    // Composing the two lanes
    // =======================================================================

    /// Heavier wins, in both directions, and the loser is REPORTED rather than
    /// dropped.
    #[test]
    fn the_heavier_lane_is_the_one_reported() {
        let mem_critical = SpawnGate::Critical(memory_observation("host", 0, GIB_U64));
        let mem_warn = SpawnGate::Warn(memory_observation("host", GIB_U64, 3 * GIB_U64));
        let thread_critical = SpawnGate::Critical(thread_observation(540, 400));
        let thread_warn = SpawnGate::Warn(thread_observation(200, 150));

        // Memory critical, threads fine ⇒ the memory refusal, nothing shadowed.
        assert_eq!(
            compose_lanes(mem_critical.clone(), SpawnGate::Proceed),
            (mem_critical.clone(), None)
        );
        // Threads critical, memory fine ⇒ the THREAD refusal. This is the
        // 2026-08-29 shape: plenty of memory, no threads left.
        assert_eq!(
            compose_lanes(SpawnGate::Proceed, thread_critical.clone()),
            (thread_critical.clone(), None)
        );
        // A thread critical outranks a memory warn…
        assert_eq!(
            compose_lanes(mem_warn.clone(), thread_critical.clone()),
            (thread_critical.clone(), Some(mem_warn.clone()))
        );
        // …and a memory critical outranks a thread warn.
        assert_eq!(
            compose_lanes(mem_critical.clone(), thread_warn.clone()),
            (mem_critical, Some(thread_warn.clone()))
        );
        // Neither lane has an opinion ⇒ nothing to report at all.
        assert_eq!(
            compose_lanes(SpawnGate::Proceed, SpawnGate::Proceed),
            (SpawnGate::Proceed, None)
        );
    }

    /// THE TIE-BREAK. On equal severity the memory lane's message is the one
    /// shown — it is the older, better-calibrated signal, and its floors are the
    /// ones the Settings panel renders. The thread lane is not discarded: it
    /// comes back as the shadowed verdict, which `probe_for_spawn` logs, so the
    /// operator is never told "low memory" while a second lane silently agreed.
    #[test]
    fn on_a_tie_the_memory_lane_is_reported_and_the_thread_lane_is_still_returned() {
        let mem_warn = SpawnGate::Warn(memory_observation("host", GIB_U64, 3 * GIB_U64));
        let thread_warn = SpawnGate::Warn(thread_observation(200, 150));
        assert_eq!(
            compose_lanes(mem_warn.clone(), thread_warn.clone()),
            (mem_warn, Some(thread_warn))
        );

        let mem_critical = SpawnGate::Critical(memory_observation("host", 0, GIB_U64));
        let thread_critical = SpawnGate::Critical(thread_observation(540, 400));
        assert_eq!(
            compose_lanes(mem_critical.clone(), thread_critical.clone()),
            (mem_critical, Some(thread_critical))
        );
    }

    /// REGRESSION TEST FOR THE INCIDENT — it fails the moment the thread lane
    /// is removed from the composition.
    ///
    /// 2026-08-29: the primary runner wedged with 540 threads (119 in
    /// `CreateProcess`) against tokio's 512-slot blocking pool, while a burst of
    /// ~130 concurrent spawns kept arriving. Free commit was NOT the binding
    /// constraint — the box had memory — so the gate as it stood admitted every
    /// one of them. Delete `evaluate_threads` from `probe_for_spawn` and this
    /// reading composes to `Proceed`, which is exactly the behaviour that let
    /// the burst land.
    #[test]
    fn a_thread_burst_is_refused_even_with_memory_to_spare() {
        let guard = defaults();

        // 32 GiB free: the memory lane has no opinion whatsoever.
        let memory = evaluate("host", Some(32 * GIB_U64), &guard);
        assert_eq!(memory, SpawnGate::Proceed);

        // 540 threads: the lane that CAN see it refuses.
        let threads = evaluate_threads(Some(540), &guard);
        let (reported, shadowed) = compose_lanes(memory, threads);
        match &reported {
            SpawnGate::Critical(o) => {
                assert_eq!(o.lane, Lane::Threads.as_str());
                assert_eq!(o.observed, 540);
                assert_eq!(o.limit, guard.critical_thread_count as u64);
            }
            other => panic!("a 540-thread process must be refused, got {other:?}"),
        }
        assert_eq!(shadowed, None, "the memory lane had no opinion to shadow");

        // And the refusal an unattended caller would receive is machine-typed
        // and names the real constraint.
        let refusal = critical_refusal("terminal session", &thread_observation(540, 400));
        assert!(refusal.starts_with(CRITICAL_REFUSAL_PREFIX));
        assert!(refusal.contains("540 threads"));
    }

    /// The asymmetry Phase 1 depends on, pinned here so it cannot drift: the
    /// SAME folded ceilings produce a WARN where a continuation should defer and
    /// a CRITICAL where a spawn should be refused. Phase 1 acts on the warn
    /// band; `admit_spawn` refuses only past the critical ceiling.
    #[test]
    fn the_warn_band_is_the_band_phase_one_defers_in() {
        let guard = merge_thread_ceilings(&defaults(), SessionFloors::default());

        // A continuation-deferring reading: over the warn ceiling, under the
        // critical one. `admit_spawn` would still let a human's terminal start.
        assert!(matches!(
            evaluate_threads(Some(300), &guard),
            SpawnGate::Warn(_)
        ));
        // A spawn-refusing reading.
        assert!(matches!(
            evaluate_threads(Some(450), &guard),
            SpawnGate::Critical(_)
        ));
        // And the band is non-empty, or the distinction would be unreachable.
        assert!(guard.critical_thread_count > guard.warn_thread_count + 1);
    }
}
