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
//! site so a future reader does not "correct" it back: the fleet term tightens
//! with a `min` ([`fold_ceiling`]) rather than a `max` ([`tighten`]); the clamp
//! that keeps the composition livable pushes UP to [`THREAD_CEILING_MIN`] (plus
//! the machine shift) rather than down to [`SESSION_FLOOR_MAX_BYTES`]; the
//! ladder invariant is `critical >= warn` ([`coerce_ceiling_ladder`]) rather
//! than `critical <= warn` ([`coerce_ladder`]); and the verdict boundary is
//! strictly ABOVE rather than strictly below.
//!
//! **One deliberate asymmetry** since plan `2026-10-01-runner-thread-ceilings-
//! ignore-the-machine-and-the-guard-dialog-says-low-memory`: on the thread lane
//! the hardcoded term is a MACHINE default (scaled with cores and memory,
//! floored at the shipped 256 / 400) and the operator's own value REPLACES it,
//! so the local knob may loosen as well as tighten — see
//! [`merge_thread_ceilings`] for why the floor lane's "nobody may loosen" rule
//! stopped being right for a quantity whose safe value depends on the box. The
//! fleet term still only tightens on both lanes, both lanes still clamp at both
//! ends, and both still fail open on an UNKNOWN reading.
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
//! The THREAD reading is the one place that promise needed defending after the
//! second lane arrived. A `GlobalMemoryStatusEx` is microseconds and stays on
//! the live path; a Windows thread snapshot walks the SYSTEM-wide thread table,
//! and [`probe_for_spawn`] is reached twice per spawn (`precheck_spawn` then
//! [`admit_spawn`]) on both the operator and the continuation path. So the
//! thread reading — and only the thread reading — is memoized for 250 ms
//! ([`crate::health_monitor::THREAD_READING_TTL`]), which collapses an admission
//! burst *and* each spawn's precheck/admit pair onto one snapshot. The memory
//! lane is deliberately NOT memoized: its freshness argument is the load-bearing
//! one on this gate, and it costs nothing to keep.
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
use crate::settings::{
    SessionGuardSettings, SHIPPED_CRITICAL_THREAD_CEILING, SHIPPED_WARN_THREAD_CEILING,
};
use qontinui_runner_lib::wedge_diagnostics::ThreadNameCensus;

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
/// "this is a real spawn failure, report it". The prefix may not change.
///
/// ## The wire after the prefix: one lane token, then human text
///
/// A refusal reads `resource_guard:critical:<metric>: <message>`, where
/// `<metric>` is [`LaneMetric::wire_name`] (`free_commit_bytes` |
/// `thread_count`) — see [`critical_refusal`]. The token is there because the
/// dialog has to know WHICH lane refused, and nothing else can tell it: a
/// refusal deliberately emits no [`RESOURCE_GUARD_EVENT`] (see that constant),
/// so the event payload's `metric` never reaches the webview for this verdict.
/// Before the token the dialog titled every refusal "Low memory", including
/// thread-lane refusals on a box with hundreds of GB free.
///
/// It goes AFTER this unchanged prefix so every consumer that matches with
/// `starts_with` keeps working byte for byte: `looping_agent_supervisor`'s
/// no-backoff arm, the external HTTP callers `mcp::tauri_proxy` tells to
/// match on the prefix, and `src/lib/resourceGuard.ts`. Everything after
/// `<metric>: ` is human text and may be reworded freely. The token is one
/// short word rather than a JSON tail because every log line and every
/// `report_spawn_failed` reason carries this string as operator-facing text.
/// The token itself is shared vocabulary with the notice payload and with
/// `src/lib/resourceGuardWire.fixture.json`, which both sides' tests read.
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
/// reason read in the other direction: [`fold_ceiling`] composes three terms,
/// none of which is bounded at its source — a hand-edited `settings.json`
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
///   ceilings (256 warn / 400 critical) or it removes the operator's room to
///   TIGHTEN: a clamp equal to the warn floor would leave nothing between the
///   clamp and the machine default, which is the "not tight, but empty" failure
///   [`SESSION_FLOOR_MAX_BYTES`]'s own doc rejects on the other lane.
///
/// 200 is therefore ~33% of headroom above the measured idle count and ~22%
/// below the shipped warn ceiling. A machine clamped all the way down to it
/// still starts sessions: 151 is not above 200.
///
/// ## The clamp actually applied is `THREAD_CEILING_MIN + shift`
///
/// Plan `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-
/// dialog-says-low-memory` (vet correction 6). This constant guarantees a
/// spawnable machine only RELATIVE to the calibration box's 151-thread idle
/// floor. Until that plan the machine shift was added AFTER the fold, which
/// carried the guarantee onto every box for free; it no longer is — an
/// operator-set or fleet ceiling is now ABSOLUTE — so on a box whose measured
/// at-rest floor is 400 a bare 200 would let an operator setting of 300 pin
/// the machine above its own idle count forever. [`merge_thread_ceilings`]
/// therefore clamps at `THREAD_CEILING_MIN + machine_thread_shift(baseline)`,
/// which keeps exactly the 49-thread margin above the measured floor that 200
/// keeps above 151, on every machine.
pub(crate) const THREAD_CEILING_MIN: usize = 200;

/// Upper bound on every effective thread ceiling, in OS threads (2048) — the
/// mirror of [`THREAD_CEILING_MIN`] at the other end.
///
/// Needed only since an operator-set ceiling can LOOSEN (plan
/// `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-dialog-
/// says-low-memory`, Phase 2): while the fold was a `min` the shipped 400 was
/// the loosest number anyone could reach, and a hand-edited 100000 was simply
/// discarded. Now it would be enforced, i.e. the lane would be off.
///
/// **Anchored, not guessed: four times tokio's 512-slot blocking pool**, the
/// bound the Settings panel already typed against as `THREAD_CEILING_INPUT_MAX`
/// — past it a ceiling could not fire before the pool it protects was already
/// exhausted. One constant on both sides (`resourceGuardHelpers.test.ts` reads
/// this one from source as `THREAD_CEILING_ABS_MAX`) rather than a fraction of
/// `threads-max`, which is Linux-only and in the millions on the 48-core box,
/// so it would bound nothing. The save command refuses a value above it rather
/// than storing a number the fold would then silently cut.
pub(crate) const THREAD_CEILING_ABS_MAX: usize = 2048;

/// The idle thread count the shipped 256 / 400 ceilings were chosen against.
///
/// [`THREAD_CEILING_MIN`]'s doc records the measurement: **150-151 threads with
/// no session running**, sampled 2026-08-30 on the Linux dev box. The HIGHER of
/// that pair is pinned here on purpose — it yields the SMALLER headroom
/// (`256 - 151 = 105`, `400 - 151 = 249`) and therefore the stricter guard, and
/// pinning one number is what makes [`machine_thread_shift`] exactly zero on
/// the machine the constants were authored for.
///
/// Do not "fix" this to 150. The pair was a range; picking either end is a
/// choice, and this is the conservative one.
pub(crate) const CALIBRATION_BASELINE: usize = 151;

/// The largest at-rest thread floor [`machine_thread_shift`] will re-base onto.
///
/// Anchored, not guessed: **512 is tokio's per-runtime `max_blocking_threads`
/// default** — the same number
/// [`qontinui_runner_lib::wedge_diagnostics::BlockingBodies::per_runtime_pool_capacity_default`]
/// publishes, and the number [`THREAD_CEILING_MIN`]'s own doc anchors the 400
/// critical ceiling to.
///
/// **It is a deliberately CONSERVATIVE cap, not the largest healthy process.**
/// The anchor is explicitly PER-RUNTIME — the field is named
/// `per_runtime_pool_capacity_default` — and this binary builds many runtimes
/// (`fleet-pub-rt`, `mcp-api-rt`, the short-lived current-thread ones), so
/// *N* runtimes carry *N* × 512 of blocking-pool capacity that is healthy by
/// tokio's own defaults. The application runtime's (`app-rt`) OWN idle pool is
/// no longer part of the floor this cap bounds: the at-rest window is fed the
/// GRADED reading (see [`record_at_rest_sample`]), which has that pool
/// subtracted already, so only the other runtimes' pools remain in the floor
/// and this cap is that much more conservative than it was. A process legitimately
/// idling above 512 therefore gets LESS shift than its floor would justify,
/// and the guard is correspondingly stricter on it. That is the intended
/// direction: this constant is chosen to under-shift rather than over-shift,
/// because the cost of under-shifting is a deferral and the cost of
/// over-shifting is an admission the machine cannot honour.
///
/// Past it the shift stops growing, so the hardcoded FLOOR of the machine
/// default caps at `512 + 105 = 617` warn and `512 + 249 = 761` critical. The
/// scaled term uses this same cap on its baseline, but is NOT capped by it as a
/// whole: its session-capacity arm grows with `per_session × capacity`, which
/// is bounded by the machine (cores, `MemTotal`) and by
/// [`MAX_THREADS_PER_SESSION`] — so a leak in a `terminal-*` family cannot
/// inflate the multiplier — and its pool arm by the live session-thread count.
/// Every ceiling is bounded by [`THREAD_CEILING_ABS_MAX`]. That bound is the
/// whole answer to "does a slow leak eventually disable this lane?" — it does
/// not: the lane keeps refusing above the cap rather than chasing the leak
/// upward, and refusing work is then correct, because the next thing to fail is
/// thread creation itself.
pub(crate) const AT_REST_BASELINE_MAX: usize = 512;

/// OS threads attributable to one live terminal session **when the census
/// cannot say** — the UNKNOWN-arm fallback, and nothing more.
///
/// Plan `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-
/// dialog-says-low-memory`, Phase 1, removed this constant's role on the live
/// path. It used to be multiplied by the live session count and subtracted
/// from every reading to estimate the at-rest floor, and on a busy box that
/// over-subtraction was not "a strictness bias" but a blind spot: 164 sessions
/// × 3 = 492 against a graded total of at most 499 took [`at_rest_estimate`]'s
/// INCOHERENT arm, the baseline read `None`, and the shift decayed to zero
/// exactly when the box was loaded. The at-rest floor is now the graded total
/// minus the threads the census NAMES as per-session
/// ([`session_thread_attribution`]). This constant survives in two places
/// only: the subtrahend on a platform with no name census (macOS, or a walk
/// that failed), and — through `min(3, MAX_THREADS_PER_SESSION)` — the input
/// to [`PER_SESSION_THREADS_FALLBACK`], where it LOSES to the family count,
/// because there it is a multiplier and the larger number would loosen.
///
/// **3, from the control's own documentation**, not from the thread-name census.
/// [`crate::agent_runtime::DEFAULT_CONTINUATION_SESSION_CAP`]'s doc derives
/// "roughly 3 OS threads per continuation session" from the 2026-08-29 wedge's
/// own arithmetic (540 threads at ~130 sessions over a 150-151 idle baseline).
/// The 2026-09-18 census finds only 2 NAMED per-session threads
/// (`terminal-waiter`, `terminal-reader`) but also shows per-session
/// `pipe-drain` and `transcript-scan`, so 2 is a floor and 3 is the measured
/// figure.
///
/// The direction of the error matters more than its size, and this is the safe
/// direction: this constant is SUBTRACTED from a live reading to estimate the
/// at-rest floor, so over-subtracting lowers the baseline, lowers the ceiling
/// and biases the guard toward STRICTNESS. Under-subtracting would let session
/// load leak into the baseline and loosen the ceiling as the box fills — the
/// guard getting weaker exactly when it is needed.
pub(crate) const THREADS_PER_SESSION: usize = 3;

/// How many observations the trailing window keeps.
///
/// At [`crate::fleet::resource_sample`]'s default 30 s tick that is nominally
/// 20 minutes — but the tick is NOT fixed (`COORD_RESOURCE_SAMPLE_SECS` has a
/// 10 s floor and no ceiling, plus ±20% jitter), so this bounds the window by
/// COUNT only. [`AT_REST_SAMPLE_MAX_AGE`] is what bounds it in TIME, and the
/// two together are what make the window's span a property of this module
/// rather than of an env var it cannot see.
///
/// The two directions this number trades between:
///
/// - LONGER is better at catching a genuinely quiet moment on a box that is
///   rarely idle, which is what makes the estimate a *floor* rather than an
///   average;
/// - SHORTER is better at letting the baseline RISE when the process's real
///   floor rises, which is the property an all-time minimum does not have at
///   all (see [`AtRestWindow`]).
pub(crate) const AT_REST_WINDOW_SAMPLES: usize = 40;

/// How old an observation may be and still count toward the floor.
///
/// **This is the bound that makes a STALLED publisher fail strict.** The
/// sample count alone cannot do it: `record_at_rest_sample` is reached only
/// from `fleet::resource_sample::collect_host_lane`, and `publish_once`
/// returns before `collect()` on three independent conditions — no
/// `machine.json`, no coord base, and no usable device JWT. Device JWTs are
/// short-lived, so the third is a routine transient rather than an exotic one.
///
/// Without an age bound, a window filled while the box was loaded or mid-leak
/// would sit there for the life of the process once the tick stopped, holding
/// the ceilings shifted (up to 617/761) off data of unbounded age — the guard
/// whose job is catching a thread leak would be the one whose loosening
/// survived longest after its telemetry died. That is UNKNOWN rendering as a
/// live measurement, which served policy
/// `verification-and-evidence` `unknown-must-not-render-as-a-default` forbids.
///
/// With it, a stalled publisher decays to `None` within 20 minutes, the shift
/// goes to zero, and the ceilings return to the shipped 256/400 — strict,
/// which is the correct direction to fail.
pub(crate) const AT_REST_SAMPLE_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(20 * 60);

/// The trailing-window low-water mark of the process's at-rest thread floor.
///
/// ## Why a WINDOW and not a running minimum
///
/// This is the whole reason the type exists, and getting it wrong reinstates
/// the defect the re-basing is meant to remove. The quantity being tracked is
/// **monotonically increasing**: measured 2026-09-18 on the 48-core box, the
/// runner started near 190 threads and sat at 424 after ~96 h, because ~227
/// blocking-pool threads accumulate and are never retired.
///
/// **An all-time minimum over a rising series is just its first sample.** It
/// would pin the baseline at the ~190 the process booted with, give a shift of
/// 39 instead of ~240, and leave a runner idling at 424 sitting in
/// `Warn(424 over 295)` — which is the exact band
/// [`crate::agent_runtime::evaluate_continuation_guard`] defers in. The latch
/// would return, unchanged in kind, with a longer fuse. A boot snapshot has the
/// same defect for the same reason.
///
/// A bounded window is what makes the floor able to move in BOTH directions: it
/// falls the moment the machine is quiet and rises as old samples age out.
///
/// ## Why one bad sample cannot pin it
///
/// Three independent bounds. [`at_rest_estimate`] refuses an incoherent
/// observation outright rather than saturating it to a low number; an accepted
/// outlier ages out of the window within [`AT_REST_WINDOW_SAMPLES`] ticks; and
/// it ages out in wall-clock time within [`AT_REST_SAMPLE_MAX_AGE`] however
/// slowly the ticks arrive. A permanent latch at a spuriously low baseline —
/// which an all-time minimum offers no recovery from short of a runner
/// restart, and served policy `production-and-cost` `runner-lifecycle` forbids
/// that restart — is therefore unreachable.
///
/// ## Why the clock is a PARAMETER
///
/// Every method takes `now` rather than reading [`std::time::Instant::now`]
/// itself, so the ageing behaviour is unit-testable without sleeping and the
/// type stays pure. [`Self::record`] and [`Self::baseline`] are the thin
/// impure wrappers the process-global uses.
#[derive(Debug, Default)]
pub(crate) struct AtRestWindow {
    /// The last [`AT_REST_WINDOW_SAMPLES`] accepted estimates as
    /// `(observed_at, at_rest)`, oldest first.
    samples: std::collections::VecDeque<(std::time::Instant, usize)>,
}

impl AtRestWindow {
    /// Fold one accepted estimate in at `now`, evicting by age and then by
    /// count. PURE with respect to the clock.
    pub(crate) fn record_at(&mut self, now: std::time::Instant, at_rest: usize) {
        self.expire(now);
        while self.samples.len() >= AT_REST_WINDOW_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back((now, at_rest));
    }

    /// The low-water mark over the samples still within
    /// [`AT_REST_SAMPLE_MAX_AGE`] of `now`, or `None` when none are.
    ///
    /// `None` is UNKNOWN and [`machine_thread_shift`] renders it as a zero
    /// shift — the shipped constants, never a permissive default.
    pub(crate) fn baseline_at(&self, now: std::time::Instant) -> Option<usize> {
        self.samples
            .iter()
            .filter(|(at, _)| Self::is_fresh(now, *at))
            .map(|(_, v)| *v)
            .min()
    }

    /// Drop every sample older than [`AT_REST_SAMPLE_MAX_AGE`].
    fn expire(&mut self, now: std::time::Instant) {
        while matches!(self.samples.front(), Some((at, _)) if !Self::is_fresh(now, *at)) {
            self.samples.pop_front();
        }
    }

    /// `saturating_duration_since`, so a clock that appears to go backwards
    /// (a sample stamped after `now`) reads as age ZERO — fresh — rather than
    /// underflowing. Freshness is the conservative reading only because the
    /// alternative here would DISCARD a just-taken sample.
    fn is_fresh(now: std::time::Instant, at: std::time::Instant) -> bool {
        now.saturating_duration_since(at) <= AT_REST_SAMPLE_MAX_AGE
    }

    /// [`Self::record_at`] at the current instant.
    pub(crate) fn record(&mut self, at_rest: usize) {
        self.record_at(std::time::Instant::now(), at_rest);
    }

    /// [`Self::baseline_at`] at the current instant.
    pub(crate) fn baseline(&self) -> Option<usize> {
        self.baseline_at(std::time::Instant::now())
    }
}

/// The process-wide [`AtRestWindow`].
///
/// A `Mutex` rather than an atomic because the value is a window rather than a
/// scalar, and the contention is one lock every 30 s against a spawn-path read.
/// A poisoned lock reads as UNKNOWN — see [`at_rest_thread_baseline`].
static AT_REST_WINDOW: Mutex<Option<AtRestWindow>> = Mutex::new(None);

/// Record one `(total threads, live sessions)` observation and fold it into the
/// at-rest floor.
///
/// Called from [`crate::fleet::resource_sample`], which already takes BOTH
/// readings two lines apart on its own 30 s publish tick — so this adds no
/// sensor, no timer and no new call into the terminal registry.
///
/// ## Where this does NOT run, stated rather than discovered
///
/// That publish path is gated: `fleet::spawn_budget_republisher` returns early
/// on a SECONDARY instance, and `resource_sample::publish_once` returns before
/// `collect()` when `~/.qontinui/machine.json` is absent, when no coord base is
/// configured, or when no device JWT resolves. On any such machine no sample is
/// ever recorded, the baseline stays `None` and the shift stays **zero** — i.e.
/// exactly the shipped 256 / 400. The inertness therefore fails toward today's
/// behaviour and never toward a permissive one, but it IS inertness: a box that
/// publishes no fleet telemetry does not get the re-basing. Sourcing the
/// baseline from a path that runs regardless of fleet publishing is recorded as
/// a follow-up on the plan rather than done here, because it needs a session
/// count outside `resource_sample` and that is a second seam.
///
/// ## Why EITHER `None` records nothing
///
/// `thread_count` is `None` on an unreadable sensor;
/// `active_terminal_sessions` is `None` when the `TerminalManager` mutex is
/// poisoned, when there is no Tauri runtime, and when there is no managed state
/// at all. Substituting `0` for the session count would INFLATE the estimated
/// floor, inflate the ceiling and loosen the guard precisely when the registry
/// is broken — an UNKNOWN rendering as a permissive default, which is the shape
/// served policy `unknown-must-not-render-as-a-default` forbids. An unusable
/// tick contributes nothing and the previous window stands.
pub(crate) fn record_at_rest_sample(total_threads: Option<usize>, live_sessions: Option<usize>) {
    // The window is fed the GRADED reading — the raw count minus the idle
    // blocking pool the census attributes to the runtime (plan
    // `2026-09-21-runner-blocking-pool-ratchets-to-peak-because-transcript-
    // tails-rotate-every-idle-thread`, Phase 3) — so the two subtractions the
    // thread lane now applies are DISJOINT: [`graded_thread_reading`] removes
    // the idle pool from the READING, and [`machine_thread_shift`] removes the
    // at-rest floor of everything else from the CEILING. Recording the raw
    // count here would fold the idle pool into the floor as well, and the
    // guard would then subtract it twice — once from the reading and once via
    // the shift — which is the composition a re-based ladder and a graded
    // reading can otherwise reach. An UNKNOWN census grades nothing out, so
    // the window degrades to the raw reading, never to a permissive one. The
    // one mixed case is transitional: a census UNKNOWN for a whole
    // AT_REST_WINDOW_SAMPLES window that then returns leaves the floor raw
    // while the reading is graded for the ticks until a graded sample enters
    // the min-window — a double subtraction bounded to that window, and the
    // reverse mix (census going UNKNOWN now) is strict. Do not "fix" it by
    // feeding the window the raw count again; that re-creates the permanent
    // double subtraction this comment exists to forbid.
    let census = crate::health_monitor::thread_name_census_memoized();
    let total_threads = total_threads.map(|total| {
        graded_thread_reading(
            total,
            census.as_ref(),
            qontinui_runner_lib::wedge_diagnostics::tracked_blocking_in_flight(),
            RUNTIME_NAMES,
            runtime_worker_threads(),
        )
        .graded
    });
    // The session subtraction is taken from the SAME census the idle pool was
    // graded out of, so the two subtractions are over one snapshot of names
    // and are disjoint by construction: the pool rows are `app-rt` /
    // `tokio-rt-worker`, the session families are `terminal-*`.
    let census_session_threads = census.as_ref().map(|c| c.session_threads);
    note_session_thread_sample(census_session_threads, live_sessions);
    let attributed = session_thread_attribution(census_session_threads, live_sessions);
    let Some(at_rest) = at_rest_estimate(total_threads, attributed) else {
        return;
    };
    if let Ok(mut guard) = AT_REST_WINDOW.lock() {
        guard
            .get_or_insert_with(AtRestWindow::default)
            .record(at_rest);
    }
}

/// How many threads one observation attributes to live sessions, or `None`
/// when that cannot be said. PURE.
///
/// Plan `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-
/// dialog-says-low-memory`, Phase 1: by NAME, from the census's uncapped
/// per-session tally ([`ThreadNameCensus::session_threads`] — the
/// `terminal-reader` / `terminal-waiter` families), not by multiplying the
/// session count by a constant. On the 2026-10-01 incident box that is the
/// difference between subtracting the 328 threads the terminals actually held
/// and subtracting 492 — more than the whole graded process.
///
/// ## The arms
///
/// - **No session count** (`live_sessions = None`): `None`. Without it neither
///   the name tally's plausibility nor the fallback can be judged, and
///   substituting `0` would inflate the floor — see [`record_at_rest_sample`].
/// - **Census present, FEWER named session threads than live terminals**
///   (`named < sessions`, which includes both families absent): `None`. Every
///   live terminal holds both threads for its whole life, so fewer than one
///   named thread per terminal is a mis-read on one side or the other — most
///   often the 30 s memoized census lagging a fresh session count after a spawn
///   burst. Under-attributing would RAISE the baseline and loosen the guard —
///   served policy `verification-and-evidence`
///   `unknown-must-not-render-as-a-default`. Same rule as
///   [`per_session_threads_from`]'s zero arm, so the two never disagree about
///   which observations are coherent.
/// - **Census present otherwise**: the named tally, as read. (Named session
///   threads with a registry reading of zero sessions is a teardown race; the
///   threads are still session threads by name, and subtracting them is the
///   strict direction.)
/// - **No census** — a platform without per-thread names (macOS), or a walk
///   that failed: the constant path, `sessions × THREADS_PER_SESSION`. 3 is
///   above the census's measured 2, so this over-subtracts, which lowers the
///   floor and is the strict direction; it is today's behaviour on those
///   platforms, unchanged.
pub(crate) fn session_thread_attribution(
    census_session_threads: Option<usize>,
    live_sessions: Option<usize>,
) -> Option<usize> {
    let sessions = live_sessions?;
    match census_session_threads {
        Some(named) if census_misread(named, sessions) => None,
        Some(named) => Some(named),
        None => Some(sessions.saturating_mul(THREADS_PER_SESSION)),
    }
}

/// `true` when a census names fewer per-session threads than there are live
/// terminals — an observation no live runner can produce, i.e. a mis-read.
/// The one predicate [`session_thread_attribution`],
/// [`per_session_threads_from`] and the census-misread provenance share.
fn census_misread(named: usize, sessions: usize) -> bool {
    named < sessions
}

/// The most threads one terminal can legitimately hold: the number of
/// per-session families (`terminal-reader`, `terminal-waiter`) — **2**.
///
/// A measured ratio above it is not a fatter terminal; it is a teardown race
/// (threads of terminals the registry already dropped) or a leak in a
/// `terminal-*` family. Both are exactly the conditions under which the guard
/// must NOT loosen, and threads-per-session is a MULTIPLIER on the
/// session-capacity arm of [`scaled_thread_ceilings`] — so it is capped here.
pub(crate) const MAX_THREADS_PER_SESSION: usize =
    qontinui_runner_lib::wedge_diagnostics::SESSION_THREAD_FAMILIES.len();

/// The threads-per-session [`scaled_thread_ceilings`] multiplies by when no
/// fresh tick measured one: `min(THREADS_PER_SESSION, MAX_THREADS_PER_SESSION)`
/// = **2**.
///
/// Not [`THREADS_PER_SESSION`]'s 3. "3 is strict" holds for the at-rest
/// baseline, where the figure is SUBTRACTED (over-subtracting lowers the
/// floor). Here it is MULTIPLIED into a ceiling, where the larger number is
/// the looser one — so the fallback takes the smaller of the two.
pub(crate) const PER_SESSION_THREADS_FALLBACK: usize =
    if THREADS_PER_SESSION < MAX_THREADS_PER_SESSION {
        THREADS_PER_SESSION
    } else {
        MAX_THREADS_PER_SESSION
    };

/// Threads per live terminal as one observation measures them, or `None` when
/// it cannot. PURE.
///
/// Integer division, rounding DOWN, and capped at [`MAX_THREADS_PER_SESSION`]:
/// this feeds the session-capacity arm of [`scaled_thread_ceilings`] as a
/// multiplier, and a smaller multiplier is a lower (stricter) ceiling — so a
/// teardown race or a leaked `terminal-*` thread can never inflate it. Fewer
/// named threads than terminals ([`census_misread`]) reads `None`, as does any
/// UNKNOWN half and a box with no terminals (nothing to divide by). `None`
/// falls back to [`PER_SESSION_THREADS_FALLBACK`] downstream.
pub(crate) fn per_session_threads_from(
    census_session_threads: Option<usize>,
    live_sessions: Option<usize>,
) -> Option<usize> {
    let (Some(named), Some(sessions)) = (census_session_threads, live_sessions) else {
        return None;
    };
    if sessions == 0 || census_misread(named, sessions) {
        return None;
    }
    Some((named / sessions).min(MAX_THREADS_PER_SESSION))
}

/// The most recent tick's measured threads-per-session, stamped with when it
/// was taken.
///
/// Recorded beside the at-rest window on the same 30 s
/// [`crate::fleet::resource_sample`] tick, for the reason that tick feeds the
/// window: it already holds the live session count, and the spawn path must
/// not take the `TerminalManager` lock to get one (see
/// `the_spawn_gate_reading_did_not_grow_the_spawn_pressure_probes`). Aged by
/// [`AT_REST_SAMPLE_MAX_AGE`] on read, so a stalled publisher decays to the
/// [`THREADS_PER_SESSION`] fallback rather than freezing a stale ratio.
static SESSION_THREAD_SAMPLE: Mutex<Option<(std::time::Instant, usize)>> = Mutex::new(None);

/// Fold one tick's `(named session threads, live sessions)` into
/// [`SESSION_THREAD_SAMPLE`]. An observation that measures nothing leaves the
/// previous sample to age out on its own rather than overwriting it.
fn note_session_thread_sample(census_session_threads: Option<usize>, live_sessions: Option<usize>) {
    // The mis-read flag is set by an incoherent tick and cleared by any tick
    // that can judge coherence and finds it, so `/health` names the state the
    // census is in NOW rather than one it was in once.
    if let (Some(named), Some(sessions)) = (census_session_threads, live_sessions) {
        if let Ok(mut flag) = SESSION_CENSUS_MISREAD.lock() {
            *flag = census_misread(named, sessions).then(std::time::Instant::now);
        }
    }
    let Some(per_session) = per_session_threads_from(census_session_threads, live_sessions) else {
        return;
    };
    if let Ok(mut slot) = SESSION_THREAD_SAMPLE.lock() {
        *slot = Some((std::time::Instant::now(), per_session));
    }
}

/// When the most recent tick last found the census contradicting the session
/// count ([`census_misread`]), or `None` when the latest judgeable tick was
/// coherent. Aged by [`AT_REST_SAMPLE_MAX_AGE`] on read like every other
/// tick-fed value.
static SESSION_CENSUS_MISREAD: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// `true` while a fresh tick reports the census as a mis-read. A poisoned lock
/// reads `false`: the flag only RELABELS provenance (the numbers are the floor
/// either way), so it is never a reason to change a verdict.
fn session_census_misread_now() -> bool {
    SESSION_CENSUS_MISREAD
        .lock()
        .ok()
        .and_then(|flag| *flag)
        .is_some_and(|at| {
            std::time::Instant::now().saturating_duration_since(at) <= AT_REST_SAMPLE_MAX_AGE
        })
}

/// The measured threads-per-session, or `None` when no fresh tick measured
/// one (or the lock is poisoned) — UNKNOWN, which
/// [`scaled_thread_ceilings`] renders as [`PER_SESSION_THREADS_FALLBACK`].
pub(crate) fn measured_per_session_threads() -> Option<usize> {
    let slot = SESSION_THREAD_SAMPLE.lock().ok()?;
    let (at, per_session) = (*slot)?;
    (std::time::Instant::now().saturating_duration_since(at) <= AT_REST_SAMPLE_MAX_AGE)
        .then_some(per_session)
}

/// The at-rest floor one `(graded total, session-attributed threads)`
/// observation implies, or `None` when the observation cannot support one.
/// PURE.
///
/// `session_threads` is [`session_thread_attribution`]'s answer — the named
/// per-session tally, or the constant path where no census exists — so this
/// function only subtracts. No multiplication happens here any more.
///
/// Split out from [`record_at_rest_sample`] so the UNKNOWN arms are settleable
/// in a test without writing to a process-global cell that every other test in
/// this binary would then share — the same purity split
/// [`merge_thread_ceilings`] keeps from [`effective_thread_ceilings`].
///
/// ## An INCOHERENT observation is UNKNOWN, not a low reading
///
/// `session_threads >= total` says the session-attributable threads are the
/// entire process, which no live runner can be: the sampler,
/// the publisher and the Tauri runtime are all unattributed to any session.
/// Such a reading is a mis-count on one side or the other — a registry read
/// taken mid-teardown, a thread sensor that undercounted — and the honest
/// answer is `None`.
///
/// Saturating it to `Some(0)` instead would be actively dangerous: a zero folds
/// into the window as a legitimate floor and drags the shift to zero for as
/// long as it survives there.
pub(crate) fn at_rest_estimate(
    total_threads: Option<usize>,
    session_threads: Option<usize>,
) -> Option<usize> {
    let (Some(total), Some(session_threads)) = (total_threads, session_threads) else {
        return None;
    };
    if session_threads >= total {
        return None;
    }
    Some(total - session_threads)
}

/// The measured at-rest thread floor, or `None` when nothing usable has been
/// sampled yet — or when the window's lock is poisoned, which is UNKNOWN for
/// the same reason and degrades to the shipped constants.
pub(crate) fn at_rest_thread_baseline() -> Option<usize> {
    AT_REST_WINDOW.lock().ok()?.as_ref()?.baseline()
}

/// How far to re-base this machine's thread ladder, in threads. PURE.
///
/// ## Why the ladder moves at all
///
/// A thread ceiling is a HEADROOM figure wearing an absolute number's clothes.
/// 256 and 400 mean "105 and 249 threads of room above a runner that idles at
/// 151" — that is how [`THREAD_CEILING_MIN`]'s doc derives them and how
/// [`crate::agent_runtime::DEFAULT_CONTINUATION_SESSION_CAP`]'s doc converts
/// them to ~35 and ~83 sessions on the calibration box. Enforced as absolutes on a machine whose idle
/// floor is 400, they are not a stricter guard: they are a LATCH. Every
/// continuation is deferred, the deferral frees ~3 threads against a floor of
/// ~400, and so the deferral cannot clear the condition that caused it.
/// Measured 2026-09-06..09-18: 616 thread-pressure-deferred continuations,
/// 11,744 deferral events, 178 never consumed.
///
/// ## What the shift does now — and what it no longer does
///
/// Until plan `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-
/// guard-dialog-says-low-memory` the shift was added to the FOLDED ceiling,
/// moving the local, fleet and hardcoded terms together. It is no longer added
/// after the fold: an operator-set or fleet ceiling is now an ABSOLUTE number,
/// and the at-rest baseline enters the machine default directly through
/// [`scaled_thread_ceilings`]. The shift survives in exactly two places, both
/// in [`merge_thread_ceilings`]:
///
/// - it raises the hardcoded FLOOR of the machine default to
///   `256 + shift` / `400 + shift`, so a box whose idle process is fatter than
///   the calibration box's never gets less headroom than it had before the
///   ceilings scaled — the latch this function was introduced to break stays
///   broken even when the scaled term is UNKNOWN;
/// - it raises the lower CLAMP to `THREAD_CEILING_MIN + shift`, so no
///   combination of absolute local and fleet terms can pin a ceiling below the
///   machine's own measured at-rest count (see [`THREAD_CEILING_MIN`]).
///
/// It is kept as this named function rather than re-derived in both places
/// because its UNKNOWN arms and its [`AT_REST_BASELINE_MAX`] cap are exactly
/// what both uses need.
///
/// ## The two UNKNOWN arms
///
/// No baseline yet, and a baseline at or below [`CALIBRATION_BASELINE`], both
/// give a shift of ZERO — a floor of the shipped 256 / 400 and a clamp at the
/// bare [`THREAD_CEILING_MIN`]. UNKNOWN degrades to the shipped constants; it
/// never degrades to a permissive default.
pub(crate) fn machine_thread_shift(baseline: Option<usize>) -> usize {
    baseline
        .map(|b| {
            b.min(AT_REST_BASELINE_MAX)
                .saturating_sub(CALIBRATION_BASELINE)
        })
        .unwrap_or(0)
}

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
    /// (`src/hooks/useResourceGuardNotifications.ts`) and the lane token on the
    /// CRITICAL refusal wire ([`CRITICAL_REFUSAL_PREFIX`], parsed by
    /// `src/lib/resourceGuard.ts`). Snake case to match the Rust field names it
    /// stands in for; the webview only ever compares it, never renders it.
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
    ///
    /// `pub(crate)` because `agent_runtime`'s continuation guard needs exactly
    /// this pair to report a deferral honestly, and the alternative — matching
    /// on the variants there and spelling `"warn"` / `"critical"` a second time
    /// — is a duplicated severity vocabulary that can drift out of step with
    /// this one. Widening the visibility keeps a single author for the word.
    pub(crate) fn tripped(&self) -> Option<(&'static str, &GateObservation)> {
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
/// an inverted pair (`critical < warn`) must resolve to the heavier verdict
/// rather than silently degrade, and the live path clamps the pair through
/// [`coerce_ceiling_ladder`] before it ever gets here.
///
/// Takes the ENFORCED pair ([`ThreadCeilings`], what [`merge_thread_ceilings`]
/// produces) rather than the settings struct: the settings' thread fields are
/// the operator's optional overrides, not limits, and judging a reading against
/// them directly is exactly the shortcut that would skip the machine default.
pub(crate) fn evaluate_threads(
    thread_count: Option<usize>,
    enabled: bool,
    ceilings: ThreadCeilings,
) -> SpawnGate {
    if !enabled {
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
    if threads > ceilings.critical {
        return SpawnGate::Critical(observation(ceilings.critical));
    }
    if threads > ceilings.warn {
        return SpawnGate::Warn(observation(ceilings.warn));
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
/// The two thread fields ride through untouched, exactly as `enabled` does —
/// they belong to a different lane with a different fleet key, folded by
/// [`merge_thread_ceilings`] into a [`ThreadCeilings`] of their own. Reading
/// `warn_thread_count` off this result would be reading the operator's
/// optional override, unfolded.
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

/// The enforced thread ceilings — the pair [`evaluate_threads`] judges a
/// reading against. Produced by [`merge_thread_ceilings`] and nothing else on
/// the live path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadCeilings {
    /// Above this many OS threads a spawn is warned about (and a gate
    /// continuation defers).
    pub(crate) warn: usize,
    /// Above this many a spawn is refused (overridably).
    pub(crate) critical: usize,
}

impl ThreadCeilings {
    /// The shipped pair, 256 / 400 — the calibration box's ceilings, and the
    /// floor of every machine's default.
    pub(crate) const SHIPPED: ThreadCeilings = ThreadCeilings {
        warn: SHIPPED_WARN_THREAD_CEILING,
        critical: SHIPPED_CRITICAL_THREAD_CEILING,
    };
}

/// Sessions per core at which the machine default WARNS (4).
///
/// **Measured, 2026-10-01, merytshost**: 164 live sessions at a load average of
/// ≈ 40 on 48 cores — about 4 sessions per core at the point where the box's
/// CPU was saturated. This is what bounds the session-capacity arm of the warn
/// ceiling: a box carrying more sessions than this per core is out of CPU, and
/// the thread count is the stand-in for that load that the guard can read
/// cheaply on the spawn path.
pub(crate) const SESSIONS_PER_CORE_WARN: usize = 4;

/// Sessions per core at which the machine default REFUSES (6) — 1.5× the
/// measured saturation point [`SESSIONS_PER_CORE_WARN`] is set at, so the
/// refusal band starts where a saturated box has taken half as much again.
pub(crate) const SESSIONS_PER_CORE_CRITICAL: usize = 6;

/// Resident memory one live session costs, in bytes (350 MiB).
///
/// **Measured, 2026-10-01, merytshost**: 320 claude/node processes holding
/// 108 GB RSS — a mean of ~346 MB each — rounded up so the memory term errs
/// toward fewer sessions. Bounds the memory arm of the session capacity:
/// `(MemTotal − reserve) / this` sessions fit in RAM.
pub(crate) const PER_SESSION_RSS_BYTES: u64 = 350 * 1024 * 1024;

/// The smallest memory reserve held back from the session-capacity sum (8 GiB)
/// — the OS, the runner itself, WSL and builds live in it.
pub(crate) const MEM_RESERVE_MIN_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// The reserve as a fraction of `MemTotal`: one tenth. The reserve is
/// `max(MEM_RESERVE_MIN_BYTES, MemTotal / 10)`, so a big box keeps a
/// proportionate cushion and a small one keeps at least 8 GiB.
pub(crate) const MEM_RESERVE_DIVISOR: u64 = 10;

/// The blocking-pool headroom of the warn ceiling, in threads: 256 − 151 =
/// **105**. NOT scaled with the machine.
///
/// The shipped ceilings protect tokio's 512-slot blocking pool
/// ([`SHIPPED_WARN_THREAD_CEILING`]'s doc), which is per-runtime and does not
/// grow with cores or RAM. So however big the box, the threads that are NOT
/// session threads get exactly the room above the at-rest floor they had on
/// the calibration box — the vet's correction 1: a scaled term that also
/// scaled this would hand a lightly loaded 48-core box ~576 threads of
/// pool headroom, more than the pool holds.
pub(crate) const POOL_HEADROOM_WARN: usize = SHIPPED_WARN_THREAD_CEILING - CALIBRATION_BASELINE;

/// The blocking-pool headroom of the critical ceiling: 400 − 151 = **249**.
/// See [`POOL_HEADROOM_WARN`].
pub(crate) const POOL_HEADROOM_CRITICAL: usize =
    SHIPPED_CRITICAL_THREAD_CEILING - CALIBRATION_BASELINE;

/// Everything about the MACHINE the thread ceilings are derived from, measured
/// once by the impure seam ([`live_thread_capacity_inputs`]) and handed to the
/// pure fold. Every field is `Option`: `None` is UNKNOWN, and each consumer
/// below says what UNKNOWN degrades to — never a permissive number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ThreadCapacityInputs {
    /// Usable cores (`available_parallelism`).
    pub(crate) cores: Option<usize>,
    /// Physical memory, bytes.
    pub(crate) mem_total_bytes: Option<u64>,
    /// The measured at-rest thread floor ([`at_rest_thread_baseline`]).
    pub(crate) baseline: Option<usize>,
    /// Measured threads per live terminal ([`measured_per_session_threads`]).
    pub(crate) per_session_threads: Option<usize>,
    /// Named per-session threads in the census right now
    /// ([`ThreadNameCensus::session_threads`]).
    pub(crate) session_threads_now: Option<usize>,
    /// The latest tick found the census naming fewer session threads than
    /// there are live terminals ([`census_misread`]). Its session tally is not
    /// to be believed, so the scaled term is UNKNOWN.
    pub(crate) session_census_misread: bool,
}

/// Why the scaled machine default could not be computed. Each arm names the
/// UNKNOWN input; the fold then uses the hardcoded floor alone, i.e. exactly
/// the pre-scaling ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScaledUnknown {
    /// The core count could not be read. `ci_node::host_sizing::probe` reports
    /// an unreadable `available_parallelism` as `1`, indistinguishable from a
    /// genuine single core, so the live seam reads `1` as UNKNOWN — the result
    /// is identical either way (a 1-core capacity falls under the floor).
    Cores,
    /// `MemTotal` could not be read.
    MemTotal,
    /// The thread-name census is UNKNOWN, so the blocking-pool arm has no
    /// session-thread count to add — and without that arm the scaled term
    /// would be the session-capacity arm alone, which is the over-loosening
    /// the `min` exists to prevent.
    SessionThreadsNow,
    /// The census named fewer session threads than there are live terminals
    /// on the latest tick — a mis-read, so its session tally is UNKNOWN even
    /// though a number was read.
    SessionCensusMisread,
}

impl ScaledUnknown {
    /// Stable machine name, for `/health` and the Settings panel.
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            ScaledUnknown::Cores => "cores_unknown",
            ScaledUnknown::MemTotal => "mem_total_unknown",
            ScaledUnknown::SessionThreadsNow => "session_threads_unknown",
            ScaledUnknown::SessionCensusMisread => "session_census_misread",
        }
    }
}

/// The scaled machine default and every intermediate it was built from, so the
/// number is explainable on `/health` rather than only enforceable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaledThreadCeilings {
    /// `min(session_arm, pool_arm)`, per field.
    pub(crate) ceilings: ThreadCeilings,
    /// `baseline + per_session_threads × session capacity` — scales with the box.
    pub(crate) session_arm: ThreadCeilings,
    /// `baseline + session_threads_now + POOL_HEADROOM_*` — constant headroom.
    pub(crate) pool_arm: ThreadCeilings,
    /// Sessions the box can carry: `min(SESSIONS_PER_CORE_* × cores, memory)`.
    pub(crate) session_capacity: ThreadCeilings,
    /// The baseline actually used: the measured floor capped at
    /// [`AT_REST_BASELINE_MAX`], or [`CALIBRATION_BASELINE`] when UNKNOWN.
    pub(crate) baseline_used: usize,
    /// The threads-per-session actually used: measured and capped at
    /// [`MAX_THREADS_PER_SESSION`], or [`PER_SESSION_THREADS_FALLBACK`] when
    /// UNKNOWN.
    pub(crate) per_session_threads_used: usize,
}

/// The machine's scaled default ceilings, or why there are none. PURE.
///
/// Plan `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-
/// dialog-says-low-memory`, §3 "Session capacity". The runner's thread count
/// measures two different things and this keeps them apart:
///
/// 1. **Session threads** (2 per terminal) are a stand-in for session LOAD,
///    whose real limits are CPU and memory — properties of the machine. So the
///    session-capacity arm scales:
///    `baseline + per_session × min(SESSIONS_PER_CORE × cores, (MemTotal − reserve) / PER_SESSION_RSS)`.
/// 2. **Everything else** is judged against tokio's 512-slot blocking pool,
///    which does NOT grow with the machine. So the pool arm keeps today's
///    headroom above the floor and the live session threads:
///    `baseline + session_threads_now + POOL_HEADROOM_*`.
///
/// The scaled ceiling is the `min` of the two (vet decision, priority
/// **robustness**): the guard trips when sessions exceed the box's capacity OR
/// when non-session threads exceed today's pool headroom, in one number, one
/// ladder and one refusal message — and the scaled term cannot reopen the
/// 2026-08-29 wedge class by handing a big, lightly loaded box more pool
/// headroom than the pool has.
///
/// UNKNOWN cores, `MemTotal` or session-thread count is `Err`, and the fold
/// falls back to the hardcoded floor — exactly the pre-scaling behaviour. An
/// UNKNOWN baseline is NOT an error: it uses [`CALIBRATION_BASELINE`], the
/// floor the shipped ceilings were derived against, and an UNKNOWN
/// per-session figure uses [`PER_SESSION_THREADS_FALLBACK`]. A measured one is
/// capped at [`MAX_THREADS_PER_SESSION`].
pub(crate) fn scaled_thread_ceilings(
    inputs: &ThreadCapacityInputs,
) -> Result<ScaledThreadCeilings, ScaledUnknown> {
    let cores = inputs.cores.ok_or(ScaledUnknown::Cores)?;
    let mem_total = inputs.mem_total_bytes.ok_or(ScaledUnknown::MemTotal)?;
    let session_threads_now = inputs
        .session_threads_now
        .ok_or(ScaledUnknown::SessionThreadsNow)?;
    if inputs.session_census_misread {
        return Err(ScaledUnknown::SessionCensusMisread);
    }
    let baseline = inputs
        .baseline
        .map(|b| b.min(AT_REST_BASELINE_MAX))
        .unwrap_or(CALIBRATION_BASELINE);
    // Capped even when "measured": the input is a plain field, and the cap is
    // what makes a leaked or racing `terminal-*` thread unable to loosen this.
    let per_session = inputs
        .per_session_threads
        .map(|n| n.min(MAX_THREADS_PER_SESSION))
        .unwrap_or(PER_SESSION_THREADS_FALLBACK);

    let reserve = MEM_RESERVE_MIN_BYTES.max(mem_total / MEM_RESERVE_DIVISOR);
    let mem_sessions = usize::try_from(mem_total.saturating_sub(reserve) / PER_SESSION_RSS_BYTES)
        .unwrap_or(usize::MAX);
    let session_capacity = ThreadCeilings {
        warn: SESSIONS_PER_CORE_WARN
            .saturating_mul(cores)
            .min(mem_sessions),
        critical: SESSIONS_PER_CORE_CRITICAL
            .saturating_mul(cores)
            .min(mem_sessions),
    };
    let session_arm = ThreadCeilings {
        warn: baseline.saturating_add(per_session.saturating_mul(session_capacity.warn)),
        critical: baseline.saturating_add(per_session.saturating_mul(session_capacity.critical)),
    };
    let pool_arm = ThreadCeilings {
        warn: baseline
            .saturating_add(session_threads_now)
            .saturating_add(POOL_HEADROOM_WARN),
        critical: baseline
            .saturating_add(session_threads_now)
            .saturating_add(POOL_HEADROOM_CRITICAL),
    };
    Ok(ScaledThreadCeilings {
        ceilings: ThreadCeilings {
            warn: session_arm.warn.min(pool_arm.warn),
            critical: session_arm.critical.min(pool_arm.critical),
        },
        session_arm,
        pool_arm,
        session_capacity,
        baseline_used: baseline,
        per_session_threads_used: per_session,
    })
}

/// Which term decided one enforced ceiling — what `/health` and the Settings
/// panel print beside the number, so a ceiling the operator did not expect
/// says where it came from instead of being the D3 confusion again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CeilingSource {
    /// The operator's own value (Settings > Resource Guard), which replaces
    /// the machine default in both directions.
    Local,
    /// The machine default, decided by the scaled term.
    Scaled,
    /// The machine default, decided by the hardcoded floor
    /// (`256 + shift` / `400 + shift`) — the scaled term was below it or
    /// UNKNOWN.
    Floor,
    /// The tenant's fleet ceiling, which only ever tightens.
    Fleet,
    /// Raised to `THREAD_CEILING_MIN + shift`: whatever was asked for sat at
    /// or under this machine's own at-rest thread count.
    ClampMin,
    /// Cut to [`THREAD_CEILING_ABS_MAX`].
    ClampMax,
    /// The critical ceiling, raised to the warn ceiling because the fold
    /// composed an inverted ladder ([`coerce_ceiling_ladder`]).
    Ladder,
    /// The hardcoded floor, because the census contradicted the session count
    /// ([`ScaledUnknown::SessionCensusMisread`]). The number is the same as
    /// [`CeilingSource::Floor`]'s; the label says it stands on an UNKNOWN, not on
    /// a sized default that happened to come out lower.
    CensusMisread,
}

impl CeilingSource {
    /// Stable machine name, for `/health` and the Settings panel
    /// (`src/components/settings/resourceGuardHelpers.ts` labels each).
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            CeilingSource::Local => "local",
            CeilingSource::Scaled => "scaled",
            CeilingSource::Floor => "floor",
            CeilingSource::Fleet => "fleet",
            CeilingSource::ClampMin => "clamp_min",
            CeilingSource::ClampMax => "clamp_max",
            CeilingSource::Ladder => "ladder",
            CeilingSource::CensusMisread => "census_misread",
        }
    }
}

/// The enforced ceilings AND the report of how they were reached — the
/// provenance vet correction 2 found missing (the old fold returned only
/// whether the ladder had been coerced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffectiveThreadCeilings {
    /// What [`evaluate_threads`] enforces.
    pub(crate) ceilings: ThreadCeilings,
    /// Which term decided [`ThreadCeilings::warn`].
    pub(crate) warn_source: CeilingSource,
    /// Which term decided [`ThreadCeilings::critical`].
    pub(crate) critical_source: CeilingSource,
    /// The ladder coercion, if the fold had to apply one — logged on a
    /// transition by [`note_ladder_coercion`].
    pub(crate) coercion: Option<LadderCoercion>,
    /// [`machine_thread_shift`] of the measured baseline.
    pub(crate) shift: usize,
    /// The hardcoded floor of the machine default: shipped + shift.
    pub(crate) floor: ThreadCeilings,
    /// The lower clamp: `THREAD_CEILING_MIN + shift`.
    pub(crate) clamp_min: usize,
    /// The scaled machine default, or why there is none.
    pub(crate) scaled: Result<ScaledThreadCeilings, ScaledUnknown>,
    /// The machine inputs the fold was given.
    pub(crate) inputs: ThreadCapacityInputs,
    /// The operator's overrides as read (`None` = machine default).
    pub(crate) local: (Option<usize>, Option<usize>),
    /// The fleet's ceilings as read (`None` = no fleet term).
    pub(crate) fleet: (Option<usize>, Option<usize>),
}

/// The thread ceilings actually enforced. PURE over its three injected terms.
///
/// Plan `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-
/// dialog-says-low-memory`, §3 "The fold", per field:
///
/// ```text
/// machine_default = max(shipped + shift, scaled?)        -- never stricter than today
/// term            = local if operator-set, else machine_default
/// effective       = clamp(min(term, fleet?), THREAD_CEILING_MIN + shift, THREAD_CEILING_ABS_MAX)
/// ```
///
/// then [`coerce_ceiling_ladder`] forces `critical >= warn`.
///
/// ## Why the operator's value REPLACES the default rather than being `min`'d
///
/// This was a `min(local, hardcoded, fleet)` — three parties who may each
/// tighten and none loosen — and that rule was right only while the hardcoded
/// term meant the same thing on every box. It does not: 256 / 400 were chosen
/// against one 151-thread idle runner, and on the 48-core box that carried 164
/// sessions on 2026-10-01 the guard told the operator to raise the limit in
/// Settings, where any value above 400 was saved and silently discarded. The
/// machine owner is the one party who sits at the box and can see what it
/// carries; their stated number is now authoritative in both directions, within
/// the two clamps that keep it from making the machine unspawnable
/// ([`THREAD_CEILING_MIN`] + shift) or the lane meaningless
/// ([`THREAD_CEILING_ABS_MAX`]).
///
/// ## Why the fleet term still only tightens
///
/// A tenant admin sets one row for machines they do not sit at; a row that
/// could loosen would loosen every box in the tenant at once, including the
/// laptop. `min` keeps the fleet term able to protect machines and unable to
/// expose them.
///
/// ## Why the clamps come last
///
/// Applied AFTER the `min`, not to each term before it, so no source can
/// escape them — a local value and a fleet column are bounded by exactly the
/// same two numbers.
pub(crate) fn merge_thread_ceilings(
    local: &SessionGuardSettings,
    fleet: SessionFloors,
    inputs: &ThreadCapacityInputs,
) -> EffectiveThreadCeilings {
    let shift = machine_thread_shift(inputs.baseline);
    let floor = ThreadCeilings {
        warn: SHIPPED_WARN_THREAD_CEILING.saturating_add(shift),
        critical: SHIPPED_CRITICAL_THREAD_CEILING.saturating_add(shift),
    };
    let clamp_min = THREAD_CEILING_MIN.saturating_add(shift);
    let scaled = scaled_thread_ceilings(inputs);
    let scaled_pair = scaled.as_ref().ok().map(|s| s.ceilings);
    let fleet_warn = fleet.warn_thread_count.map(|n| n as usize);
    let fleet_critical = fleet.critical_thread_count.map(|n| n as usize);

    let (warn, warn_source) = fold_ceiling(
        local.warn_thread_count,
        floor.warn,
        scaled_pair.map(|p| p.warn),
        fleet_warn,
        clamp_min,
    );
    let (requested_critical, mut critical_source) = fold_ceiling(
        local.critical_thread_count,
        floor.critical,
        scaled_pair.map(|p| p.critical),
        fleet_critical,
        clamp_min,
    );
    let (critical, coercion) = coerce_ceiling_ladder(warn, requested_critical);
    if coercion.is_some() {
        critical_source = CeilingSource::Ladder;
    }
    // Same numbers, honest label: a floor that won only because the census
    // was a mis-read is UNKNOWN-backed, not a sized default.
    let relabel = |source: CeilingSource| match (&scaled, source) {
        (Err(ScaledUnknown::SessionCensusMisread), CeilingSource::Floor) => {
            CeilingSource::CensusMisread
        }
        _ => source,
    };
    let warn_source = relabel(warn_source);
    let critical_source = relabel(critical_source);
    EffectiveThreadCeilings {
        ceilings: ThreadCeilings { warn, critical },
        warn_source,
        critical_source,
        coercion,
        shift,
        floor,
        clamp_min,
        scaled,
        inputs: *inputs,
        local: (local.warn_thread_count, local.critical_thread_count),
        fleet: (fleet_warn, fleet_critical),
    }
}

/// One field of [`merge_thread_ceilings`]: the value and the term that decided
/// it. PURE.
///
/// The fleet `None` arm is spelled out rather than folded as `unwrap_or(0)`,
/// which would be the wrong statement: an absent fleet ceiling is UNKNOWN, and
/// on a `min` a zero wins outright and refuses every spawn on the machine. A
/// future edit that wants to treat UNKNOWN as a value has to delete this arm to
/// do it. The same holds for the scaled term: `None` contributes nothing and
/// the floor stands.
fn fold_ceiling(
    local: Option<usize>,
    floor: usize,
    scaled: Option<usize>,
    fleet: Option<usize>,
    clamp_min: usize,
) -> (usize, CeilingSource) {
    let machine_default = match scaled {
        Some(scaled) if scaled > floor => (scaled, CeilingSource::Scaled),
        _ => (floor, CeilingSource::Floor),
    };
    let mut term = match local {
        Some(operator) => (operator, CeilingSource::Local),
        None => machine_default,
    };
    if let Some(fleet_ceiling) = fleet {
        if fleet_ceiling < term.0 {
            term = (fleet_ceiling, CeilingSource::Fleet);
        }
    }
    if term.0 < clamp_min {
        (clamp_min, CeilingSource::ClampMin)
    } else if term.0 > THREAD_CEILING_ABS_MAX {
        (THREAD_CEILING_ABS_MAX, CeilingSource::ClampMax)
    } else {
        term
    }
}

impl EffectiveThreadCeilings {
    /// The `/health` `threadCeilings` block and the Settings panel's
    /// `thread_ceilings` payload — one projection, so the two surfaces cannot
    /// describe the same ceilings differently. camelCase keys; every UNKNOWN is
    /// JSON `null` with its reason named, never a zero.
    pub(crate) fn to_json(&self, enabled: bool) -> serde_json::Value {
        let pair =
            |p: ThreadCeilings| serde_json::json!({ "warn": p.warn, "critical": p.critical });
        let (scaled, scaled_unknown) = match &self.scaled {
            Ok(s) => (
                serde_json::json!({
                    "warn": s.ceilings.warn,
                    "critical": s.ceilings.critical,
                    "sessionArm": pair(s.session_arm),
                    "poolArm": pair(s.pool_arm),
                    "sessionCapacity": pair(s.session_capacity),
                    "baselineUsed": s.baseline_used,
                    "perSessionThreadsUsed": s.per_session_threads_used,
                }),
                serde_json::Value::Null,
            ),
            Err(why) => (serde_json::Value::Null, serde_json::json!(why.wire_name())),
        };
        serde_json::json!({
            "enabled": enabled,
            "warn": self.ceilings.warn,
            "critical": self.ceilings.critical,
            "provenance": {
                "warn": self.warn_source.wire_name(),
                "critical": self.critical_source.wire_name(),
            },
            "local": { "warn": self.local.0, "critical": self.local.1 },
            "fleet": { "warn": self.fleet.0, "critical": self.fleet.1 },
            "floor": pair(self.floor),
            "shift": self.shift,
            "clampMin": self.clamp_min,
            "absMax": THREAD_CEILING_ABS_MAX,
            "scaled": scaled,
            "scaledUnknown": scaled_unknown,
            "inputs": {
                "cores": self.inputs.cores,
                "memTotalBytes": self.inputs.mem_total_bytes,
                "baseline": self.inputs.baseline,
                "perSessionThreads": self.inputs.per_session_threads,
                "sessionThreadsNow": self.inputs.session_threads_now,
                "sessionCensusMisread": self.inputs.session_census_misread,
            },
            "ladderCoerced": self.coercion.is_some(),
        })
    }
}

/// A ladder the three-term fold inverted, and what it was clamped to. Reported
/// by [`merge_floors_reporting`] / [`merge_thread_ceilings`], logged
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
/// That is the state
/// `commands::resource_guard_settings::save_session_guard_settings` refuses to
/// persist locally — through `thread_ceilings_are_inverted`, the MIRROR of the
/// floors' predicate and not a second call to it — so, exactly as on the memory
/// lane, what a local writer refuses to store a remote term must not be able to
/// synthesise.
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

/// This host's cores and `MemTotal`, probed ONCE per process.
///
/// `ci_node::host_sizing::probe` is the shipped capability probe (plan
/// `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-git-
/// spawns-are-ungated`'s `fleet::machine_capability` is the preferred input
/// once it lands; reusing a shipped probe rather than writing a third is the
/// point). It does a blocking sysinfo refresh, and [`effective_thread_ceilings`]
/// runs on every spawn, so it is cached in a `OnceLock` — neither quantity
/// changes under a live process in any way this guard should chase.
fn host_capacity() -> crate::ci_node::host_sizing::HostCapacity {
    static HOST: std::sync::OnceLock<crate::ci_node::host_sizing::HostCapacity> =
        std::sync::OnceLock::new();
    *HOST.get_or_init(crate::ci_node::host_sizing::probe)
}

/// The machine inputs for [`merge_thread_ceilings`], read live. IMPURE — the
/// cached host probe, the at-rest window, the per-session sample — and pure in
/// the census, which the caller passes so a verdict and its ceilings are judged
/// off ONE census snapshot.
///
/// `cpus <= 1` is read as UNKNOWN cores: `host_sizing::probe` reports an
/// unreadable `available_parallelism` as `1`, and the provenance must name that
/// as `cores_unknown` rather than present it as a measurement. A genuine
/// single-core box loses nothing by it — its scaled term falls under the floor
/// either way.
fn live_thread_capacity_inputs(census: Option<&ThreadNameCensus>) -> ThreadCapacityInputs {
    let host = host_capacity();
    ThreadCapacityInputs {
        cores: (host.cpus > 1).then_some(host.cpus as usize),
        mem_total_bytes: host.mem_bytes,
        baseline: at_rest_thread_baseline(),
        per_session_threads: measured_per_session_threads(),
        session_threads_now: census.map(|c| c.session_threads),
        session_census_misread: session_census_misread_now(),
    }
}

/// The effective thread ceilings: [`merge_thread_ceilings`] over the caller's
/// local settings, the fleet's cached ceilings and this machine's live inputs.
/// The mirror of [`effective_session_floors`], and the same single impure seam.
///
/// Takes no lane, because there is only one: the thread count is a property of
/// **this process**, not of a host/WSL resource pool, so it has exactly one set
/// of limits and looks them up under [`Lane::Threads`] — the shared lane
/// vocabulary rather than a `"threads"` literal, so a rename is a compile error
/// instead of a silently empty lookup.
///
/// That lookup returns nothing today and is expected to: **coord publishes no
/// thread column**, so the fleet term is dormant, which is exactly the poller's
/// documented fail-safe for a term it has never received.
///
/// Reads the memoized census itself; [`thread_lane_verdict`], which already
/// holds one, goes through [`effective_thread_ceilings_given`] instead.
pub(crate) fn effective_thread_ceilings(local: &SessionGuardSettings) -> EffectiveThreadCeilings {
    let census = crate::health_monitor::thread_name_census_memoized();
    effective_thread_ceilings_given(local, census.as_ref())
}

/// [`effective_thread_ceilings`] over a census the caller already holds.
fn effective_thread_ceilings_given(
    local: &SessionGuardSettings,
    census: Option<&ThreadNameCensus>,
) -> EffectiveThreadCeilings {
    let lane = Lane::Threads.as_str();
    let effective = merge_thread_ceilings(
        local,
        crate::mcp::fleet_policy_poller::fleet_session_floors(lane),
        // The machine inputs are the SECOND impure read this seam owns, and
        // they belong here for the same reason the fleet cache does:
        // `merge_thread_ceilings` is pure and must stay settleable in a test.
        &live_thread_capacity_inputs(census),
    );
    note_ladder_coercion(lane, effective.coercion);
    effective
}

/// The `/health` `threadCeilings` block: the enforced thread ceilings, every
/// input they were derived from, and the term that decided each — see
/// [`EffectiveThreadCeilings::to_json`]. A thread ceiling the operator cannot
/// read is the confusion plan `2026-10-01-runner-thread-ceilings-ignore-the-
/// machine-and-the-guard-dialog-says-low-memory` D3 records; this is where it
/// becomes readable. Computed live on each call (no I/O beyond the settings
/// read and the 30 s census memo).
pub(crate) fn thread_ceilings_health_json() -> serde_json::Value {
    let local = crate::settings::get_session_guard_settings();
    effective_thread_ceilings(&local).to_json(local.enabled)
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

/// The most idle-pool threads [`graded_thread_reading`] will ever subtract.
///
/// tokio's blocking pool is capped at `max_blocking_threads`, whose default is
/// **512** (`runtime::Builder::new` sets `max_blocking_threads: 512` for every
/// flavor; `runtime/builder.rs:288` on the pinned tokio 1.50.0), so a census
/// can never legitimately attribute more than `workers + 512` threads to one
/// runtime. This clamp protects against a census that OVER-REPORTS — a second
/// runtime sharing the name, a mis-tallied walk — and nothing else: in
/// production `named - workers - in_flight` is at most 512 by construction,
/// so the clamp is not reachable from a correct census and it is NOT what
/// keeps a saturated pool counted.
///
/// **What this grading cannot see, stated so nobody reads the clamp as
/// cover.** A pool thread inside an UNTRACKED body — `tokio::fs`,
/// `tokio::process`, and every raw `tokio::task::spawn_blocking` site that
/// does not take a `BlockingSlot` — is indistinguishable from an idle one
/// here, because `in_flight` counts tracked bodies only. 512 pool threads
/// all stuck in untracked `CreateProcess` calls (the 2026-08-29 wedge shape)
/// would therefore grade out entirely and the lane would read `Proceed`.
/// The guard is strict against TRACKED bodies (they stay counted); the
/// untracked residue is closed only by coverage — converting raw
/// `spawn_blocking` sites to `spawn_blocking_tracked` — which is the recorded
/// follow-up on plan `2026-09-21-runner-blocking-pool-ratchets-to-peak-
/// because-transcript-tails-rotate-every-idle-thread`, not by any number
/// here.
pub(crate) const IDLE_POOL_SUBTRAHEND_CAP: usize = 512;

/// The thread names the application runtime's scheduler workers and blocking
/// pool carry, in the order this binary has shipped them: `tokio-rt-worker` is
/// tokio's default and what the Tauri runtime is named today; `app-rt` is the
/// name plan `2026-09-18-the-runner-thread-pressure-guard-is-a-latch-not-
/// back-pressure` PR 1 gives it. Both are listed so the grading survives that
/// rename without a code change here; a census never carries both at once.
pub(crate) const RUNTIME_NAMES: &[&str] = &["app-rt", "tokio-rt-worker"];

/// The application runtime's scheduler worker count — the part of a
/// [`RUNTIME_NAMES`] row that is NOT the blocking pool.
///
/// Plan `2026-09-18-…-latch-not-back-pressure` PR 1 (`db96c48f3`) builds the
/// Tauri runtime with `crate::app_runtime_worker_threads()` workers —
/// `min(available_parallelism, 16)` or the bounded env override — under the
/// name `app-rt`. This reads the SAME resolver, so the subtrahend and the
/// runtime can never disagree about how many of the named threads are
/// scheduler workers rather than pool. Reading `available_parallelism()` here
/// instead (the pre-PR-1 shape) over-counted workers by 16 on a 32-core box,
/// which under-graded the pool — the strict direction, but wrong.
pub(crate) fn runtime_worker_threads() -> usize {
    crate::app_runtime_worker_threads()
}

/// A thread reading with the runtime's IDLE blocking pool graded out of it
/// (plan `2026-09-21-runner-blocking-pool-ratchets-to-peak-because-transcript-
/// tails-rotate-every-idle-thread`, Phase 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GradedThreadReading {
    /// The raw OS thread count, as [`crate::health_monitor::thread_count_reading`]
    /// measured it.
    pub(crate) total: usize,
    /// How many threads were graded out: named runtime threads beyond the
    /// scheduler workers and beyond the tracked in-flight bodies, capped at
    /// [`IDLE_POOL_SUBTRAHEND_CAP`].
    pub(crate) idle_pool: usize,
    /// `total − idle_pool` — the number the ceilings are judged against.
    pub(crate) graded: usize,
}

/// PURE: grade an idle blocking pool out of a raw thread count.
///
/// `named` is the census count for the [`RUNTIME_NAMES`] rows (an UNKNOWN
/// census is `0` named — the guard then degrades to today's strict reading,
/// never to a permissive one, served policy `verification-and-evidence`
/// `unknown-must-not-render-as-a-default`). Of those, `worker_threads` are the
/// scheduler and `in_flight` are pool threads genuinely inside a tracked body,
/// and both keep counting as load; what is left is the idle pool, subtracted
/// up to [`IDLE_POOL_SUBTRAHEND_CAP`]. Every step saturates, so no input can
/// make `graded` exceed `total` or underflow.
pub(crate) fn graded_thread_reading(
    total: usize,
    census: Option<&ThreadNameCensus>,
    in_flight: usize,
    runtime_names: &[&str],
    worker_threads: usize,
) -> GradedThreadReading {
    let named: usize = census
        .map(|c| {
            c.by_name
                .iter()
                .filter(|row| runtime_names.contains(&row.name.as_str()))
                .map(|row| row.count)
                .sum()
        })
        .unwrap_or(0);
    let idle_pool = named
        .saturating_sub(worker_threads)
        .saturating_sub(in_flight)
        .min(IDLE_POOL_SUBTRAHEND_CAP);
    GradedThreadReading {
        total,
        idle_pool,
        graded: total.saturating_sub(idle_pool),
    }
}

/// Edge-triggered log of the thread lane's graded trip, so an operator reading
/// a refusal that says "carrying 150 threads" beside a `/health` that says 441
/// finds the line that reconciles the two. One line per change of severity
/// (a trip, an escalation, and again after a clear), never one per poll: the
/// continuation guard asks this lane every few minutes for every pending
/// continuation, and `idle_pool` drifts by a few threads between polls.
static LAST_GRADED_TRIP: Mutex<Option<&'static str>> = Mutex::new(None);

fn note_graded_trip(verdict: &SpawnGate, reading: &GradedThreadReading) {
    let key = verdict.tripped().map(|(severity, _)| severity);
    let mut last = LAST_GRADED_TRIP.lock().unwrap_or_else(|e| e.into_inner());
    if *last == key {
        return;
    }
    *last = key;
    if let Some((severity, obs)) = verdict.tripped() {
        warn!(
            lane = %obs.lane,
            severity = severity,
            threads = reading.total,
            idle_pool = reading.idle_pool,
            graded = reading.graded,
            limit = obs.limit,
            "resource_guard: {}",
            graded_trip_message(severity, reading, obs.limit),
        );
    }
}

/// The reconciling sentence: raw, idle and graded on one line, beside the
/// ceiling that was crossed. PURE so the wording is pinned by a test.
fn graded_trip_message(severity: &str, reading: &GradedThreadReading, limit: u64) -> String {
    format!(
        "the thread lane tripped on the GRADED reading — {} threads, {} idle blocking-pool \
         threads graded out, graded {} against the {}-thread {} ceiling",
        reading.total, reading.idle_pool, reading.graded, limit, severity,
    )
}

/// The thread lane's live verdict, folded and evaluated — [`probe_for_spawn`]'s
/// thread half.
///
/// **The single call site of
/// [`crate::health_monitor::thread_count_reading_memoized`]**, which is what
/// keeps the whole lane — the continuation guard, [`precheck_spawn`] and
/// [`admit_spawn`] alike — behind one system-wide thread snapshot per 250 ms
/// window. Reaching past it to `thread_count_reading` from a second site would
/// silently restore the per-caller snapshot this seam exists to remove; see that
/// constant's doc for why the staleness is free.
///
/// The reading handed to [`evaluate_threads`] is the GRADED one
/// ([`graded_thread_reading`]): the raw count minus the runtime's idle
/// blocking pool, which the 30 s thread-name census
/// ([`crate::health_monitor::thread_name_census_memoized`]) makes visible and
/// [`qontinui_runner_lib::wedge_diagnostics::tracked_blocking_in_flight`] keeps
/// honest. `evaluate_threads` and the ceilings it compares against are
/// untouched; the `GateObservation.observed` every refusal quotes is the
/// graded number, and [`note_graded_trip`] logs the raw one beside it.
fn thread_lane_verdict(local: &SessionGuardSettings) -> SpawnGate {
    let census = crate::health_monitor::thread_name_census_memoized();
    let ceilings = effective_thread_ceilings_given(local, census.as_ref()).ceilings;
    let Some(total) = crate::health_monitor::thread_count_reading_memoized() else {
        return evaluate_threads(None, local.enabled, ceilings);
    };
    let reading = graded_thread_reading(
        total,
        census.as_ref(),
        qontinui_runner_lib::wedge_diagnostics::tracked_blocking_in_flight(),
        RUNTIME_NAMES,
        runtime_worker_threads(),
    );
    let verdict = evaluate_threads(Some(reading.graded), local.enabled, ceilings);
    note_graded_trip(&verdict, &reading);
    verdict
}

/// Live verdict: read the limits, take one reading per lane, evaluate both,
/// report the heavier.
///
/// **Also the entry point for the unattended admission guards** —
/// `agent_runtime::evaluate_continuation_guard` and `admit_launch` inject this
/// function, so the queue and the seam cannot disagree about which lanes count
/// (a thread-lane-only `thread_pressure` used to sit there, and on Windows let
/// the memory lane refuse at the seam after a check that never consulted it —
/// plan `2026-09-30-a-gate-continuation-is-claimed-before-the-resource-guard-and-the-claude-cli-check`,
/// D1). Those callers act on a DIFFERENT severity from [`admit_spawn`], which
/// is why this returns the whole [`SpawnGate`] rather than a bool:
///
/// - A **gate continuation** (or coord launch) defers at [`SpawnGate::Warn`].
///   It can wait and be re-delivered, nobody is sitting in front of it, and
///   back-pressure that arrives early is the entire point of a queue.
/// - An **operator's own spawn** is refused only at [`SpawnGate::Critical`], and
///   even then overridably ([`admit_spawn`]). Refusing a human's terminal on a
///   soft signal is the false positive this module's doctrine ranks worst.
///
/// So: match on the verdict, act at the severity your caller's cost of waiting
/// justifies. Do not invent a second set of thresholds — the numbers are folded
/// once, from settings and the fleet. Side effects are logs only (the
/// shadowed-lane `warn!` below and [`note_graded_trip`]'s edge-triggered one);
/// the webview notice is [`admit_spawn`]'s alone.
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
/// The thread reading is
/// [`crate::health_monitor::thread_count_reading_memoized`] — the same in-process
/// OS-table read the health monitor has made every 60 s since it shipped (no
/// subprocess, no WMI, no allocation beyond the walk), taken at most once per
/// [`crate::health_monitor::THREAD_READING_TTL`]. This function is reached TWICE
/// per spawn on both paths (`precheck_spawn` then `admit_spawn` for an operator
/// terminal; the continuation guard then `admit_spawn` for a continuation), and
/// on Windows the walk is of the SYSTEM-wide thread table — so without the memo
/// an admission burst lands one system-wide snapshot per caller, contending with
/// the very `CreateProcess` calls this gate protects.
///
/// The fleet terms are folded in AFTER the readings, because the lane to look
/// the memory floors up under comes from the reading itself.
pub(crate) fn probe_for_spawn() -> SpawnGate {
    let local = crate::settings::get_session_guard_settings();
    if !local.enabled {
        return SpawnGate::Proceed;
    }
    // The reading now carries free PHYSICAL alongside free commit — one
    // `GlobalMemoryStatusEx` fills both, so the second figure costs nothing.
    // Phase 1 of plan `2026-08-08-memory-floors-watch-commit-and-physical` is
    // behaviour-neutral BY CONSTRUCTION: the verdict below still consults the
    // commit figure and only the commit figure. Phase 2 is what gives the
    // physical reading a floor of its own; do not add one here.
    let (lane, free_commit_bytes, _free_phys_bytes) =
        crate::fleet::resource_sample::spawn_gate_reading();
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
///
/// The lane token ([`LaneMetric::wire_name`]) sits between the prefix and the
/// text, so the dialog can title the refusal by the lane that actually spoke —
/// see [`CRITICAL_REFUSAL_PREFIX`] for why it cannot come from anywhere else.
fn critical_refusal(what: &str, observation: &GateObservation) -> String {
    format!(
        "{CRITICAL_REFUSAL_PREFIX}{}: Not starting a new {what}: {}. {} The limits live in \
         Settings > Resource Guard.",
        observation.metric.wire_name(),
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
    admit_spawn_observed(what, resource_override, app).map_err(|refusal| refusal.message)
}

/// An un-overridden CRITICAL refusal from [`admit_spawn_observed`]: the exact
/// text [`admit_spawn`] returns, beside the observation that produced it.
///
/// The observation travels with the text so an unattended caller can classify
/// the refusal STRUCTURALLY — which lane, what reading, against what limit —
/// instead of parsing the operator-facing sentence back apart (plan
/// `2026-09-30-a-gate-continuation-is-claimed-before-the-resource-guard-and-the-claude-cli-check`, D5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CriticalRefusal {
    /// [`CRITICAL_REFUSAL_PREFIX`]-tagged, byte-identical to [`admit_spawn`]'s.
    pub(crate) message: String,
    pub(crate) observation: GateObservation,
}

/// [`admit_spawn`] with the refusal kept typed. Same verdict, same notices,
/// same log lines — `admit_spawn` is this function with the observation
/// dropped.
pub(crate) fn admit_spawn_observed(
    what: &str,
    resource_override: bool,
    app: Option<&AppHandle>,
) -> Result<(), CriticalRefusal> {
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
            Err(CriticalRefusal {
                message: critical_refusal(what, &observation),
                observation,
            })
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
/// `GlobalMemoryStatusEx` call plus — at most once per
/// [`crate::health_monitor::THREAD_READING_TTL`], and in practice never here,
/// because [`admit_spawn`] takes the same reading milliseconds later — one
/// thread-table walk, and leaks nothing.
///
/// **Both lanes run here, deliberately.** Narrowing this pre-check to the memory
/// lane to save the walk would reintroduce the leak for every THREAD-lane
/// refusal: the thread ceilings are the lane that fires first on the path to a
/// wedge (~35 concurrent sessions at the warn ceiling), so it is the lane whose
/// refusals repeat, and each one would land after a `git worktree add` and a
/// coord claim. The memo is what makes paying for both lanes here free; dropping
/// a lane is not.
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

// ===========================================================================
// Background-work shedding — rung 1 of the degradation ladder
// ===========================================================================
//
// Plan `2026-09-23-resource-guard-floors-are-constants-and-the-runners-own-git-spawns-are-ungated`,
// Phase 3 ("shed the runner's own periodic work first").
//
// Everything above this line gates a NEW session. Nothing gated the runner's
// OWN periodic work — `worktree_census`, `fleet::tree_publisher`,
// `git_status_subset`, the auto-fresh engine, `build_drift` and the transcript
// WMI enumeration — and on the MSI box that work is the `git` burst (≈2,500
// spawns per census tick, uncapped `2N`/s commit-state probes) that preceded
// every one of the four commit-exhaustion aborts. Under
// `ERROR_COMMITMENT_LIMIT` each of those spawns failed, logged one WARN and was
// folded into a `Degraded`, and **the loop did not slow down**. The guard asked
// the operator to give something up before the runner gave up anything.
//
// This section is the missing rung: the cheapest one on the ladder, and the
// only one that costs the user nothing when it fires, because every spender it
// gates is periodic and idempotent — the next cycle recomputes from scratch.
//
// Three rules shape it, each argued at its item:
//
// - **One verdict, many call sites.** [`background_work_verdict`] reads the SAME
//   free-commit reading the spawn gate reads, against the SAME effective floors
//   ([`effective_session_floors`]), and maps [`evaluate`]'s three verdicts onto
//   Run / Throttle / Skip. No threshold is invented here; a machine whose owner
//   tightened the floors sheds earlier for exactly that reason.
// - **Gate the LOOP, never the helper.** `run_probe` and `git_trunk` also serve
//   on-demand paths the operator is waiting on (worktree allocation, the probe
//   executor). Shedding those would turn a memory-pressure signal into a
//   silently missing answer; shedding a periodic tick costs one tick.
// - **UNKNOWN runs as today.** An unreadable sensor, a disabled guard, and a box
//   with no commit concept (every non-Windows box) all produce
//   [`BackgroundWork::Run`] — the gate's fail-open posture, unchanged.
//
// And the ladder's one prohibition holds here too: nothing in this section
// touches a live session. It only declines to START a unit of the runner's own
// background work.

/// What the runner's own periodic work should do this tick.
///
/// The mapping from [`SpawnGate`] is one-to-one — Proceed → Run, Warn →
/// Throttle, Critical → Skip — and deliberately so: the floors a spawn is
/// warned at are the floors background work starts yielding at. What each
/// spender DOES with a Throttle is its own decision (the census and tree
/// publisher run one tick in four; the cheap re-checks skip the tick;
/// `git_status_subset` lengthens its per-session window), which is why this is
/// a verdict rather than a boolean.
///
/// The observation rides along so the one line a spender logs on entering the
/// shed state can say WHICH floor, at WHICH reading — the same clause the spawn
/// gate's toast uses, not a second vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BackgroundWork {
    /// Enough headroom, or no readable opinion (UNKNOWN / guard disabled). Run
    /// exactly as before this rung existed.
    Run,
    /// Free commit is below the WARN floor. Do less.
    Throttle(GateObservation),
    /// Free commit is below the CRITICAL floor. Do nothing this tick.
    Skip(GateObservation),
}

impl BackgroundWork {
    /// The observation behind a shed verdict, with its severity word, or `None`
    /// for [`BackgroundWork::Run`].
    fn tripped(&self) -> Option<(&'static str, &GateObservation)> {
        match self {
            BackgroundWork::Run => None,
            BackgroundWork::Throttle(o) => Some(("warn", o)),
            BackgroundWork::Skip(o) => Some(("critical", o)),
        }
    }
}

/// Pure verdict over an injected free-commit reading and effective floors.
///
/// Built ON [`evaluate`] rather than beside it, so the three properties that
/// function already argues — strictly-below boundaries, critical tested first,
/// and fail-open on both `None` and `enabled == false` — are inherited rather
/// than restated. A second comparison against the same floors is how the two
/// would one day disagree about which side of a floor a reading is on.
pub(crate) fn background_work_verdict_for(
    lane: &str,
    free_commit_bytes: Option<u64>,
    floors: &SessionGuardSettings,
) -> BackgroundWork {
    match evaluate(lane, free_commit_bytes, floors) {
        SpawnGate::Proceed => BackgroundWork::Run,
        SpawnGate::Warn(observation) => BackgroundWork::Throttle(observation),
        SpawnGate::Critical(observation) => BackgroundWork::Skip(observation),
    }
}

/// How long [`background_work_verdict`] reuses the machine owner's guard
/// SETTINGS before re-reading them.
///
/// The spawn gate reads them through `get_session_guard_settings()`, a full
/// `load_settings()` — which can persist `settings.json`, touch
/// `claude-accounts.json` and reach the OS keyring. That is fine at a spawn, a
/// few times an hour; it is not fine at the head of `git_status_subset`'s emit,
/// which runs up to `2N` times a second across N sessions, and it would be doing
/// that work most at exactly the moment this verdict exists for — a box running
/// out of commit. So this seam reads through the NON-WRITING
/// [`crate::settings::read_settings_from_disk`] (mtime-cached, no overlays; the
/// guard section has none to miss) and memoises the result. The settings change
/// when an operator edits the Settings panel, so a few seconds of lag in when a
/// changed floor starts shedding background work is invisible.
///
/// The READING is deliberately not memoised — the same choice the spawn gate
/// makes for the memory lane: it is one `GlobalMemoryStatusEx` (microseconds, no
/// allocation), and its freshness is the whole argument for consulting it.
const BACKGROUND_SETTINGS_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// The memoised guard settings behind [`BACKGROUND_SETTINGS_TTL`].
static BACKGROUND_SETTINGS: Mutex<Option<(std::time::Instant, SessionGuardSettings)>> =
    Mutex::new(None);

/// The live verdict for the runner's own background work: this instant's
/// free-commit reading against the effective session floors.
///
/// Reads [`crate::fleet::resource_sample::spawn_gate_reading`] — the spawn
/// gate's own reading, host lane, one syscall — and the floors through
/// [`effective_session_floors`], so the three-term fold, the cap and the ladder
/// coercion all apply exactly as they do at a spawn. Called at the HEAD of each
/// spender's tick, never inside `run_probe` / `git_trunk` (see the section
/// header for why).
///
/// Never call this from a unit test: it reads the operator's real settings.
/// Every shedding decision below takes the verdict as an argument precisely so a
/// test can inject one.
pub(crate) fn background_work_verdict() -> BackgroundWork {
    let memoised = BACKGROUND_SETTINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(read_at, _)| read_at.elapsed() < BACKGROUND_SETTINGS_TTL)
        .map(|(_, settings)| settings.clone());
    let local = match memoised {
        Some(settings) => settings,
        None => {
            // The disk read happens OUTSIDE the lock, so a slow disk stalls only
            // this caller, never every other spender's tick behind the mutex.
            // Two callers racing a stale memo both read and the later swap wins
            // — both values are the same file's truth.
            let settings = crate::settings::read_settings_from_disk()
                .settings
                .session_guard;
            *BACKGROUND_SETTINGS
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some((std::time::Instant::now(), settings.clone()));
            settings
        }
    };
    if !local.enabled {
        return BackgroundWork::Run;
    }
    let (lane, free_commit_bytes, _free_phys_bytes) =
        crate::fleet::resource_sample::spawn_gate_reading();
    let floors = effective_session_floors(&local, lane);
    background_work_verdict_for(lane, free_commit_bytes, &floors)
}

/// What one spender actually did on its last decision — the state the
/// edge-triggered log line reports a CHANGE of.
///
/// Richer than [`BackgroundWork`] because a spender's behaviour is not a pure
/// function of the verdict: a walk that is holding off after a critical episode
/// is shedding under a `Run` verdict, and that has to be logged as its own state
/// or the "resumed" line would be a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShedState {
    /// Doing its work exactly as before this rung existed.
    Running,
    /// Doing its work, but less of it (a longer debounce, say).
    Throttled,
    /// Not doing its work this tick.
    Skipped,
    /// Pressure has cleared, but the spender is still waiting out its backoff.
    HoldingOff,
}

/// Edge-triggered logger for one spender's shed state.
///
/// The `note_ladder_coercion` discipline, applied per spender: remember the last
/// state logged and emit ONLY on a change. Unconditional per-tick logging is the
/// exact failure this rung exists to end — the census alone put ≈6,000 WARN
/// lines a day into the log while the box was dying — so a shed that repeated
/// its line every tick would re-create the flood through the mechanism meant to
/// stop it. A spender that stays shed for an hour logs one line on entry and one
/// on recovery.
///
/// One instance per spender, owned by whoever owns the spender's state: a loop
/// holds it in its own stack frame (so its tests see only their own lines), and
/// the two spenders that are not loops (`git_status_subset`, the WMI command)
/// hold one in a `static Mutex`.
#[derive(Debug)]
pub(crate) struct ShedLog {
    spender: &'static str,
    state: ShedState,
}

impl ShedLog {
    /// A logger that starts in [`ShedState::Running`], so the FIRST shed is
    /// reported and a spender that never sheds never logs. `const` so it can
    /// initialise a `static`.
    pub(crate) const fn new(spender: &'static str) -> Self {
        Self {
            spender,
            state: ShedState::Running,
        }
    }

    /// Record that the spender is now in `next`, logging one line if — and
    /// only if — that is a change. Returns whether a line was emitted.
    ///
    /// `verdict` supplies the reading and floor the line quotes; on a return to
    /// [`ShedState::Running`] it is not consulted, because the line that matters
    /// there is "resumed", and the reading that permitted it is not above any
    /// floor worth naming.
    pub(crate) fn note(&mut self, next: ShedState, verdict: &BackgroundWork) -> bool {
        if self.state == next {
            return false;
        }
        let previous = std::mem::replace(&mut self.state, next);
        let spender = self.spender;
        let why = verdict
            .tripped()
            .map(|(severity, obs)| obs.clause(severity))
            .unwrap_or_else(|| "free commit is back above the warn floor".to_string());
        match next {
            ShedState::Running => tracing::info!(
                spender,
                "resource_guard: {spender} resumed its background work (was {previous:?}) — {why}"
            ),
            ShedState::Throttled => warn!(
                spender,
                "resource_guard: throttling {spender}'s background work — {why}. The runner sheds \
                 its own periodic work before it asks the operator to give anything up; this \
                 line is logged once per change, not per tick"
            ),
            ShedState::Skipped => warn!(
                spender,
                "resource_guard: skipping {spender}'s background work — {why}. Nothing it does \
                 is lost: it is periodic and recomputes from scratch on the next cycle that \
                 runs; this line is logged once per change, not per tick"
            ),
            ShedState::HoldingOff => tracing::info!(
                spender,
                "resource_guard: free commit is no longer below the critical floor, but \
                 {spender} is holding off a few more cycles before resuming (exponential backoff \
                 after a critical episode) — {why}"
            ),
        }
        true
    }
}

/// How a periodic spender answers a [`BackgroundWork::Throttle`] and
/// [`BackgroundWork::Skip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShedPolicy {
    /// Skip the tick at Throttle and at Skip, and resume the moment the verdict
    /// is Run again. For the cheap re-checks (the auto-fresh engine, 300 s;
    /// `build_drift`, 900 s): a handful of `git` calls, where the backoff's
    /// extra staleness would buy nothing.
    SkipTick,
    /// At Throttle, run one tick in [`SHED_THROTTLE_RUN_EVERY`]; at Skip, skip
    /// AND back off exponentially (capped) so the walk does not restart on the
    /// first tick the reading pokes back above the floor. For the two big walks
    /// — the census (~12–15 `git` per worktree row) and the tree publisher (~9
    /// per repo plus a `git fetch`) — whose burst is itself enough to push an
    /// oscillating box back under.
    ///
    /// WARN reduces the RATE rather than stopping the work: a box can sit below
    /// its warn floor for hours (the floors are constants, not a measure of this
    /// machine — the defect this plan's later phases address), and a census or a
    /// tree table that simply stops for that long is a coord view that silently
    /// stops describing the box. Only CRITICAL — the band the four aborts were
    /// in — stops it outright.
    ThrottleAndBackoff,
}

/// At a sustained [`BackgroundWork::Throttle`], a
/// [`ShedPolicy::ThrottleAndBackoff`] loop runs one tick in this many (skips
/// three of four): a quarter of the burst rate, while the census still refreshes
/// every 20 min and the tree table every 4 min.
pub(crate) const SHED_THROTTLE_RUN_EVERY: u32 = 4;

/// Cap on [`ShedPolicy::ThrottleAndBackoff`]'s hold-off, in cycles.
///
/// The hold-off doubles with each consecutive critical tick — 1, 2, 4 — and
/// stops at 4. Four is where the extra staleness stops being cheap: for the
/// publisher (60 s) it is four minutes of stale tree rows; for the census
/// (300 s) twenty minutes, and the census has an on-demand refresh
/// (`spawn_census_rebuild`) that this rung does not gate, so an operator who
/// needs it sooner is never waiting on the backoff. Past four, a box that has
/// genuinely recovered is being punished for a past episode.
pub(crate) const SHED_BACKOFF_MAX_CYCLES: u32 = 4;

/// Per-loop shedding state: the policy, the edge-triggered logger, and the
/// backoff and throttle counters. Owned by the loop, so two loops (or two tests)
/// never share a streak.
#[derive(Debug)]
pub(crate) struct BackgroundShed {
    policy: ShedPolicy,
    log: ShedLog,
    /// Consecutive ticks that saw [`BackgroundWork::Skip`]. Reset when a cycle
    /// actually runs.
    critical_streak: u32,
    /// Cycles still to skip once the verdict is no longer Skip.
    holdoff: u32,
    /// Consecutive Throttle ticks past the hold-off; every
    /// [`SHED_THROTTLE_RUN_EVERY`]th one runs.
    throttle_ticks: u32,
}

impl BackgroundShed {
    pub(crate) const fn new(spender: &'static str, policy: ShedPolicy) -> Self {
        Self {
            policy,
            log: ShedLog::new(spender),
            critical_streak: 0,
            holdoff: 0,
            throttle_ticks: 0,
        }
    }

    /// Decide whether THIS tick runs, given this tick's verdict. Logs a line
    /// only when the decision's state changes — a throttled loop that runs one
    /// tick in four stays in [`ShedState::Throttled`] throughout, so its skipped
    /// and its run ticks do not alternate lines.
    ///
    /// The hold-off is spent by every tick that is not critical, Throttle as
    /// well as Run: a box that dips critical once and then sits in the warn
    /// band for hours must come back to the reduced rate, not stay stopped
    /// because the hold-off only drains on a fully healthy reading.
    pub(crate) fn admit(&mut self, verdict: &BackgroundWork) -> bool {
        let (run, state) = match verdict {
            BackgroundWork::Skip(_) => {
                if self.policy == ShedPolicy::ThrottleAndBackoff {
                    self.critical_streak = self.critical_streak.saturating_add(1);
                    let doubling = 1u32
                        .checked_shl(self.critical_streak.saturating_sub(1))
                        .unwrap_or(u32::MAX);
                    self.holdoff = doubling.min(SHED_BACKOFF_MAX_CYCLES);
                }
                self.throttle_ticks = 0;
                (false, ShedState::Skipped)
            }
            _ if self.holdoff > 0 => {
                self.holdoff -= 1;
                (false, ShedState::HoldingOff)
            }
            BackgroundWork::Throttle(_) => match self.policy {
                ShedPolicy::SkipTick => (false, ShedState::Skipped),
                ShedPolicy::ThrottleAndBackoff => {
                    self.throttle_ticks = self.throttle_ticks.saturating_add(1);
                    let run = self.throttle_ticks % SHED_THROTTLE_RUN_EVERY == 0;
                    if run {
                        self.critical_streak = 0;
                    }
                    (run, ShedState::Throttled)
                }
            },
            BackgroundWork::Run => {
                self.critical_streak = 0;
                self.throttle_ticks = 0;
                (true, ShedState::Running)
            }
        };
        self.log.note(state, verdict);
        run
    }
}

/// Test-only: run `f` with a scoped `tracing` subscriber and return what it
/// logged at INFO and above. Scoped (`with_default`) rather than global because
/// the harness runs tests in parallel and a global subscriber can be set once.
/// Shared by every module whose shedding wiring asserts "exactly one line".
#[cfg(test)]
pub(crate) fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Sink;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let sink = Sink::default();
    let buf = sink.0.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(sink)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned();
    (out, text)
}

/// Test-only: the two shed verdicts, at a reading below the named floor of the
/// shipped defaults. For the modules that inject a verdict into their own tick.
#[cfg(test)]
pub(crate) fn test_skip_verdict() -> BackgroundWork {
    background_work_verdict_for("host", Some(0), &SessionGuardSettings::default())
}

#[cfg(test)]
pub(crate) fn test_throttle_verdict() -> BackgroundWork {
    let floors = SessionGuardSettings::default();
    // Midway between the two floors: below warn, at-or-above critical.
    let between = (floors.warn_free_commit_bytes + floors.critical_free_commit_bytes) / 2;
    background_work_verdict_for("host", Some(between), &floors)
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

    /// The shipped 256 / 400 pair, enforced as-is — what a machine with no
    /// measured inputs and no overrides gets.
    fn shipped() -> ThreadCeilings {
        ThreadCeilings::SHIPPED
    }

    /// A machine whose only known input is its at-rest baseline: cores and
    /// `MemTotal` UNKNOWN, so the scaled term is `Err` and the machine default
    /// is the hardcoded floor `shipped + machine_thread_shift(baseline)`.
    fn machine(baseline: Option<usize>) -> ThreadCapacityInputs {
        ThreadCapacityInputs {
            baseline,
            ..ThreadCapacityInputs::default()
        }
    }

    /// The fold over a [`machine`] with the given baseline.
    fn fold(
        local: &SessionGuardSettings,
        fleet: SessionFloors,
        baseline: Option<usize>,
    ) -> EffectiveThreadCeilings {
        merge_thread_ceilings(local, fleet, &machine(baseline))
    }

    /// Operator overrides for the two thread ceilings, everything else default.
    fn local_threads(warn: Option<usize>, critical: Option<usize>) -> SessionGuardSettings {
        SessionGuardSettings {
            warn_thread_count: warn,
            critical_thread_count: critical,
            ..defaults()
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
    /// `src/lib/resourceGuard.ts` matches on: prefix first, then the lane
    /// token, then the lane, the live headroom and the configured floor.
    #[test]
    fn refusal_names_the_prefix_the_lane_the_headroom_and_the_floor() {
        let msg = critical_refusal(
            "terminal session",
            &memory_observation("host", 1_073_741_824, 1_610_612_736),
        );
        assert!(msg.starts_with(CRITICAL_REFUSAL_PREFIX));
        assert!(
            msg.starts_with("resource_guard:critical:free_commit_bytes: "),
            "missing lane token: {msg}"
        );
        assert!(msg.contains("terminal session"));
        assert!(msg.contains("host lane"));
        assert!(msg.contains("1.00 GiB"), "missing headroom: {msg}");
        assert!(msg.contains("1.50 GiB"), "missing floor: {msg}");
    }

    /// The wire fixture both sides read, byte for byte. `src/lib/resourceGuard.test.ts`
    /// parses these exact strings; this test proves Rust still produces them.
    /// Read from the manifest dir, never the CWD, so the test binary finds it
    /// wherever it runs from.
    fn wire_fixture() -> serde_json::Value {
        let raw = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/lib/resourceGuardWire.fixture.json"
        ));
        serde_json::from_str(raw).expect("resourceGuardWire.fixture.json is valid JSON")
    }

    /// Every lane's refusal carries its [`LaneMetric::wire_name`] token
    /// straight after the unchanged prefix, and the full string is exactly
    /// what the webview's parser is tested against. A rewording here without
    /// the matching fixture edit fails this test, which is the point: the
    /// dialog's title depends on the token surviving every rewording.
    #[test]
    fn the_refusal_wire_matches_the_shared_fixture() {
        let fixture = wire_fixture();
        assert_eq!(fixture["prefix"], CRITICAL_REFUSAL_PREFIX);

        let cases = [
            (
                LaneMetric::FreeCommitBytes,
                memory_observation("host", GIB_U64, 3 * GIB_U64 / 2),
            ),
            (LaneMetric::ThreadCount, thread_observation(540, 400)),
        ];
        for (metric, observation) in cases {
            let wire = critical_refusal("terminal session", &observation);
            let token = format!("{CRITICAL_REFUSAL_PREFIX}{}: ", metric.wire_name());
            assert!(
                wire.starts_with(&token),
                "{metric:?} lost its token: {wire}"
            );
            assert_eq!(
                fixture["refusals"][metric.wire_name()],
                wire.as_str(),
                "{metric:?}: the Rust refusal and src/lib/resourceGuardWire.fixture.json \
                 disagree — update both sides together"
            );
        }
    }

    /// The dialog's titles are the Rust headlines, held in the shared fixture
    /// so the webview cannot drift from the toast and log vocabulary.
    #[test]
    fn the_lane_headlines_match_the_shared_fixture() {
        let fixture = wire_fixture();
        for metric in [LaneMetric::FreeCommitBytes, LaneMetric::ThreadCount] {
            assert_eq!(
                fixture["headlines"][metric.wire_name()],
                metric.headline(),
                "{metric:?}"
            );
        }
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
                evaluate_threads(Some(threads), true, shipped()),
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
        assert_eq!(evaluate_threads(None, true, shipped()), SpawnGate::Proceed);
    }

    /// FAIL OPEN #2, thread lane. One switch covers both lanes, so a disabled
    /// guard has no opinion at any thread count — including the 540 the wedged
    /// process actually carried, and a count no machine could reach.
    #[test]
    fn disabled_guard_proceeds_at_every_thread_count() {
        for threads in [None, Some(0), Some(151), Some(540), Some(1_000_000)] {
            assert_eq!(
                evaluate_threads(threads, false, shipped()),
                SpawnGate::Proceed
            );
        }
    }

    /// Between the ceilings ⇒ warn, carrying the numbers the message quotes.
    #[test]
    fn between_the_ceilings_warns_and_reports_both_numbers() {
        let g = shipped();
        assert_eq!(
            evaluate_threads(Some(300), true, g),
            SpawnGate::Warn(thread_observation(300, g.warn as u64))
        );
    }

    /// Above the critical ceiling ⇒ critical. 540 is the count the wedged
    /// process carried on 2026-08-29.
    #[test]
    fn above_the_critical_ceiling_is_critical() {
        let g = shipped();
        assert_eq!(
            evaluate_threads(Some(540), true, g),
            SpawnGate::Critical(thread_observation(540, g.critical as u64))
        );
    }

    /// Boundaries are STRICTLY above — the mirror of the floor lane's strictly
    /// below, and for the same reason: a machine sitting exactly ON its ceiling
    /// is at the ceiling, not over it, and quoting "the 150-thread warn
    /// ceiling" while warning at exactly 150 makes the displayed number a lie
    /// by one thread.
    #[test]
    fn exactly_at_a_ceiling_does_not_trip_it() {
        let g = shipped();
        assert_eq!(
            evaluate_threads(Some(g.warn), true, g),
            SpawnGate::Proceed,
            "exactly at the warn ceiling is not past it"
        );
        match evaluate_threads(Some(g.warn + 1), true, g) {
            SpawnGate::Warn(o) => assert_eq!(o.observed, g.warn as u64 + 1),
            other => panic!("expected Warn one thread over the warn ceiling, got {other:?}"),
        }
        match evaluate_threads(Some(g.critical), true, g) {
            SpawnGate::Warn(o) => assert_eq!(o.observed, g.critical as u64),
            other => panic!("expected Warn exactly at the critical ceiling, got {other:?}"),
        }
        match evaluate_threads(Some(g.critical + 1), true, g) {
            SpawnGate::Critical(o) => assert_eq!(o.limit, g.critical as u64),
            other => {
                panic!("expected Critical one thread over the critical ceiling, got {other:?}")
            }
        }
    }

    /// A transposed pair reaching the pure verdict must still resolve to the
    /// heavier verdict, for the same reason it does on the floor lane (the live
    /// path coerces the ladder first; this pins the verdict on its own).
    #[test]
    fn inverted_ceilings_resolve_to_the_heavier_verdict() {
        let inverted = ThreadCeilings {
            warn: 400,
            critical: 150,
        };
        match evaluate_threads(Some(200), true, inverted) {
            SpawnGate::Critical(o) => assert_eq!(o.limit, 150),
            other => panic!("expected Critical under transposed ceilings, got {other:?}"),
        }
    }

    // ---- The graded reading (plan 2026-09-21-runner-blocking-pool-ratchets-
    // to-peak-because-transcript-tails-rotate-every-idle-thread, Phase 3) ----
    //
    // `evaluate_threads` above is untouched: these pin what is FED to it.

    fn name_census(rows: &[(&str, usize)]) -> ThreadNameCensus {
        use qontinui_runner_lib::wedge_diagnostics::ThreadNameCount;
        ThreadNameCensus {
            total: rows.iter().map(|(_, n)| n).sum(),
            by_name: rows
                .iter()
                .map(|(name, count)| ThreadNameCount {
                    name: name.to_string(),
                    count: *count,
                })
                .collect(),
            session_threads: rows
                .iter()
                .filter(|(name, _)| name.starts_with("terminal-"))
                .map(|(_, n)| n)
                .sum(),
            sampled_at: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    /// The operator-box case: 440 threads of which 325 are `tokio-rt-worker`,
    /// 32 scheduler workers, 3 bodies in flight ⇒ 290 idle pool threads are
    /// graded out and the 150 that remain PROCEED under the shipped 256/400
    /// ceilings — where the raw 440 was a refusal.
    #[test]
    fn an_idle_pool_is_graded_out_and_the_remainder_proceeds() {
        let census = name_census(&[("tokio-rt-worker", 325), ("terminal-reader-*", 19)]);
        let r = graded_thread_reading(440, Some(&census), 3, RUNTIME_NAMES, 32);
        assert_eq!(
            r,
            GradedThreadReading {
                total: 440,
                idle_pool: 290,
                graded: 150,
            }
        );
        assert_eq!(
            evaluate_threads(Some(r.graded), true, shipped()),
            SpawnGate::Proceed
        );
        assert!(matches!(
            evaluate_threads(Some(r.total), true, shipped()),
            SpawnGate::Critical(_)
        ));
    }

    /// UNKNOWN census ⇒ nothing is graded out ⇒ today's strict reading, and
    /// today's `Critical`. Never the permissive direction.
    #[test]
    fn an_unknown_census_grades_nothing_out() {
        let r = graded_thread_reading(440, None, 3, RUNTIME_NAMES, 32);
        assert_eq!(r.idle_pool, 0);
        assert_eq!(r.graded, 440);
        assert!(matches!(
            evaluate_threads(Some(r.graded), true, shipped()),
            SpawnGate::Critical(_)
        ));
    }

    /// The clamp bites: 600 named beyond 32 workers is 568 idle, but only
    /// [`IDLE_POOL_SUBTRAHEND_CAP`] (512) may be subtracted, so a pool at
    /// tokio's own cap still counts.
    /// The re-based ladder (`machine_thread_shift`) and the graded reading
    /// subtract DISJOINT components: the at-rest window is fed the graded
    /// reading, so the idle pool is never in the floor the shift is derived
    /// from. Feeding it the RAW count would double-subtract — the shift would
    /// carry the pool a second time. Pinned as arithmetic over the two pure
    /// halves, because the live seam (`record_at_rest_sample`) writes a
    /// process-global window.
    #[test]
    fn the_idle_pool_is_subtracted_once_not_twice() {
        // The operator box: 441 threads, 325 named pool rows, 16 pinned
        // workers, 3 tracked bodies in flight, 12 live sessions — 36 session
        // threads on the constant path (this census names no terminal rows).
        let census = name_census(&[("tokio-rt-worker", 325)]);
        let graded = graded_thread_reading(441, Some(&census), 3, RUNTIME_NAMES, 16);
        assert_eq!(graded.idle_pool, 306);
        assert_eq!(graded.graded, 135);

        // What the window sees when fed the GRADED reading: the floor of the
        // non-pool threads, so the shift is derived from 135 - 36 = 99, which
        // is below CALIBRATION_BASELINE and shifts NOTHING.
        let floor_graded = at_rest_estimate(Some(graded.graded), Some(36));
        assert_eq!(floor_graded, Some(99));
        assert_eq!(machine_thread_shift(floor_graded), 0);

        // What it would have seen fed the RAW count: the pool folded into the
        // floor, a shift of ~254 on top of a reading that already excludes the
        // same 306 threads — the double subtraction this composition forbids.
        let floor_raw = at_rest_estimate(Some(441), Some(36));
        assert_eq!(floor_raw, Some(405));
        assert!(machine_thread_shift(floor_raw) > 200);
    }

    #[test]
    fn the_subtrahend_is_capped_at_the_pool_ceiling() {
        let census = name_census(&[("tokio-rt-worker", 600)]);
        let r = graded_thread_reading(700, Some(&census), 0, RUNTIME_NAMES, 32);
        assert_eq!(r.idle_pool, IDLE_POOL_SUBTRAHEND_CAP);
        assert_eq!(r.idle_pool, 512);
        assert_eq!(r.graded, 188);
    }

    /// A pool thread inside a tracked body keeps counting as load: 50 in
    /// flight on the operator-box case raises the graded reading by exactly 50.
    #[test]
    fn in_flight_bodies_stay_counted() {
        let census = name_census(&[("tokio-rt-worker", 325)]);
        let idle = graded_thread_reading(440, Some(&census), 3, RUNTIME_NAMES, 32);
        let busy = graded_thread_reading(440, Some(&census), 53, RUNTIME_NAMES, 32);
        assert_eq!(idle.graded, 150);
        assert_eq!(busy.graded, 200);
        assert_eq!(busy.idle_pool, 240);
        // The delta is the in-flight delta, one for one.
        let fifty = graded_thread_reading(440, Some(&census), 50, RUNTIME_NAMES, 32);
        assert_eq!(fifty.graded, 197);
    }

    /// The reconciling WARN names all three numbers and the ceiling, so a
    /// refusal quoting the graded count and a `/health` quoting the raw one
    /// can be read as the same measurement.
    #[test]
    fn the_graded_trip_message_carries_raw_idle_and_graded() {
        let reading = GradedThreadReading {
            total: 441,
            idle_pool: 291,
            graded: 150,
        };
        let msg = graded_trip_message("warn", &reading, 256);
        assert!(msg.contains("441 threads"), "{msg}");
        assert!(msg.contains("291 idle blocking-pool threads"), "{msg}");
        assert!(msg.contains("graded 150"), "{msg}");
        assert!(msg.contains("256-thread warn ceiling"), "{msg}");
    }

    /// A census naming only unrelated threads grades nothing out — the
    /// subtraction is keyed on the runtime names, never on "most threads".
    #[test]
    fn unrelated_thread_names_are_not_a_pool() {
        let census = name_census(&[
            ("terminal-reader-*", 200),
            ("notify-rs windows loop", 19),
            ("<unnamed>", 40),
        ]);
        let r = graded_thread_reading(440, Some(&census), 3, RUNTIME_NAMES, 32);
        assert_eq!(r.idle_pool, 0);
        assert_eq!(r.graded, r.total);
    }

    /// Both runtime names count (the rename in the 09-18 plan's PR 1 must not
    /// switch the grading off), and every arithmetic step saturates: a
    /// census that claims more named threads than the total exists cannot
    /// underflow the graded reading.
    #[test]
    fn the_grading_survives_the_runtime_rename_and_never_underflows() {
        let renamed = name_census(&[("app-rt", 325)]);
        assert_eq!(
            graded_thread_reading(440, Some(&renamed), 3, RUNTIME_NAMES, 32).graded,
            150
        );
        let impossible = name_census(&[("tokio-rt-worker", 900)]);
        let r = graded_thread_reading(100, Some(&impossible), 0, RUNTIME_NAMES, 32);
        assert_eq!(r.idle_pool, 512);
        assert_eq!(r.graded, 0);
        // More workers than named threads: nothing to subtract, no underflow.
        let small = name_census(&[("tokio-rt-worker", 10)]);
        let r = graded_thread_reading(100, Some(&small), 0, RUNTIME_NAMES, 32);
        assert_eq!(r.idle_pool, 0);
        assert_eq!(r.graded, 100);
    }

    /// The thread lane names itself through the shared lane vocabulary, never a
    /// literal — the same rule the fleet-limit cache's `for_lane` depends on.
    #[test]
    fn the_thread_lane_uses_the_shared_lane_name() {
        match evaluate_threads(Some(10_000), true, shipped()) {
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
        assert!(
            msg.starts_with("resource_guard:critical:thread_count: "),
            "a thread refusal must name its lane, or the dialog says \"Low memory\": {msg}"
        );
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

        // Same for the thread lane, whose fleet term is likewise limits-only:
        // the fold produces ceilings and nothing else, and the verdict reads
        // the owner's switch.
        let merged = fold(&off, fleet_threads(Some(10), Some(20)), None);
        assert_eq!(
            evaluate_threads(Some(9_999), off.enabled, merged.ceilings),
            SpawnGate::Proceed
        );
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
    // The effective CEILING: the operator's value or the machine default,
    // tightened by the fleet, clamped at both ends
    // =======================================================================

    /// One field of the fold, every arm, in the direction each term may move.
    #[test]
    fn fold_ceiling_takes_the_right_term_in_each_direction() {
        use CeilingSource::*;
        // No override: the machine default — the floor, unless the scaled
        // term is above it.
        assert_eq!(fold_ceiling(None, 400, None, None, 200), (400, Floor));
        assert_eq!(fold_ceiling(None, 400, Some(350), None, 200), (400, Floor));
        assert_eq!(fold_ceiling(None, 400, Some(747), None, 200), (747, Scaled));
        // An operator value REPLACES the default, in BOTH directions.
        assert_eq!(
            fold_ceiling(Some(220), 400, Some(747), None, 200),
            (220, Local)
        );
        assert_eq!(
            fold_ceiling(Some(1000), 400, None, None, 200),
            (1000, Local)
        );
        // The fleet only ever tightens — past the operator or the default —
        // and a looser fleet value changes nothing.
        assert_eq!(fold_ceiling(None, 400, None, Some(250), 200), (250, Fleet));
        assert_eq!(
            fold_ceiling(Some(1000), 400, None, Some(600), 200),
            (600, Fleet)
        );
        assert_eq!(
            fold_ceiling(Some(300), 400, None, Some(390), 200),
            (300, Local)
        );
        assert_eq!(fold_ceiling(None, 400, None, Some(1500), 200), (400, Floor));
        // The clamps come last, so no source escapes them.
        assert_eq!(
            fold_ceiling(Some(100), 400, None, None, 251),
            (251, ClampMin)
        );
        assert_eq!(fold_ceiling(None, 400, None, Some(0), 251), (251, ClampMin));
        assert_eq!(
            fold_ceiling(Some(100_000), 400, None, None, 200),
            (THREAD_CEILING_ABS_MAX, ClampMax)
        );
    }

    /// An ABSENT fleet term contributes NOTHING — it is not a zero, and the
    /// arithmetic difference is the whole machine: on a `min`, `0` wins outright
    /// and would refuse every spawn on the box. This is the arm that runs
    /// today, since coord publishes no thread column at all.
    #[test]
    fn an_absent_fleet_ceiling_contributes_nothing() {
        let merged = fold(&defaults(), SessionFloors::default(), None);
        assert_eq!(
            merged.ceilings,
            shipped(),
            "the dormant fleet term must leave a default machine exactly as it was"
        );
        assert_eq!(merged.warn_source, CeilingSource::Floor);
        assert_eq!(merged.critical_source, CeilingSource::Floor);
        // …and a zero fleet ceiling, which IS a statement, is still bounded by
        // the clamp rather than taken literally.
        let pinned = fold(&defaults(), fleet_threads(Some(0), Some(0)), None);
        assert_eq!(pinned.ceilings.warn, THREAD_CEILING_MIN);
        assert_eq!(pinned.warn_source, CeilingSource::ClampMin);
    }

    // ------------------------------------------------------------------
    // The latch, and the safety valve. Plan
    // `2026-09-18-the-runner-thread-pressure-guard-is-a-latch-not-back-pressure`.
    // ------------------------------------------------------------------

    /// The effective ceilings for a machine whose at-rest floor is `baseline`
    /// and whose scaled term is UNKNOWN — the floor alone, which is what these
    /// latch tests were written against and must still hold.
    fn ceilings_for(baseline: Option<usize>) -> ThreadCeilings {
        fold(&defaults(), SessionFloors::default(), baseline).ceilings
    }

    /// **ARM A — the latch. This is the test that fails on `origin/main`.**
    ///
    /// A 48-core runner measured 2026-09-18 (device `eb2155ed`): 424 OS
    /// threads, ZERO sessions running, an at-rest floor of ~402. Against the
    /// absolute 400 this returns `Critical(424 over 400)` and
    /// `agent_runtime::evaluate_continuation_guard` defers every gate
    /// continuation — 616 of them over 12 days, 11,744 deferral events, 178
    /// never consumed.
    ///
    /// It is a LATCH rather than back-pressure because the deferral frees
    /// `THREADS_PER_SESSION` threads against a floor of ~402: the act the guard
    /// takes cannot move the quantity the guard read. With no sessions running
    /// there is not even that.
    #[test]
    fn a_high_core_runner_at_rest_does_not_defer() {
        let merged = ceilings_for(Some(402));
        assert_eq!(
            evaluate_threads(Some(424), true, merged),
            SpawnGate::Proceed,
            "a runner sitting at its own at-rest floor with zero sessions must \
             not be refused: {merged:?}"
        );
    }

    /// **ARM B — the safety valve.** A latch is being fixed, not a guard
    /// removed. Genuine load on the same machine still refuses.
    #[test]
    fn a_genuinely_loaded_high_core_runner_still_defers() {
        let merged = ceilings_for(Some(402));
        // 402 - 151 = 251, so the re-based ladder is 507 / 651. Assert the
        // LIMIT, not `o.observed`: `observed` is the argument echoed back
        // unchanged, so asserting it would hold for every ceiling including the
        // un-shifted one, and this arm would pass with the fix reverted.
        assert_eq!(merged.warn, 507);
        assert_eq!(merged.critical, 651);
        match evaluate_threads(Some(700), true, merged) {
            SpawnGate::Critical(o) => assert_eq!((o.observed, o.limit), (700, 651)),
            other => panic!("expected Critical far above the re-based ceiling, got {other:?}"),
        }
        // ARM B' — and the WARN band still exists between the two, which is the
        // band the continuation guard actually defers on.
        match evaluate_threads(Some(520), true, merged) {
            SpawnGate::Warn(o) => assert_eq!((o.observed, o.limit), (520, 507)),
            other => panic!("expected Warn inside the re-based warn band, got {other:?}"),
        }
    }

    /// **ARM C — the machine the constants were calibrated on is EXACTLY
    /// unchanged**, not approximately. `shift` is zero at and below
    /// [`CALIBRATION_BASELINE`], so this is the shipped 256 / 400 byte for
    /// byte — which is what makes the re-basing safe to ship to a fleet whose
    /// other boxes were never mis-bounded.
    #[test]
    fn the_calibrated_box_is_byte_identical() {
        for baseline in [
            None,
            Some(0),
            Some(120),
            Some(150),
            Some(CALIBRATION_BASELINE),
        ] {
            let merged = ceilings_for(baseline);
            assert_eq!(
                merged.warn, SHIPPED_WARN_THREAD_CEILING,
                "baseline {baseline:?} must not move the warn ceiling"
            );
            assert_eq!(
                merged.critical, SHIPPED_CRITICAL_THREAD_CEILING,
                "baseline {baseline:?} must not move the critical ceiling"
            );
            assert_eq!(
                evaluate_threads(Some(300), true, merged),
                SpawnGate::Warn(thread_observation(300, 256))
            );
        }
    }

    /// **ARM D — no baseline is UNKNOWN, and UNKNOWN keeps the shipped
    /// constants.** It must never render as a permissive default: a machine
    /// that cannot measure its own floor gets the guard it has today, not a
    /// weaker one.
    #[test]
    fn an_unknown_baseline_keeps_the_shipped_ceilings() {
        assert_eq!(machine_thread_shift(None), 0);
        let merged = ceilings_for(None);
        match evaluate_threads(Some(424), true, merged) {
            SpawnGate::Critical(o) => assert_eq!(o.limit, 400),
            other => panic!("UNKNOWN must fall back to the shipped 400, got {other:?}"),
        }
    }

    /// **ARM E — the bound holds, so a leak cannot disable the lane.**
    ///
    /// The baseline is capped at [`AT_REST_BASELINE_MAX`] (tokio's per-runtime
    /// blocking-pool default), so the ceilings cap too. Without this the
    /// low-water mark would chase a monotone leak upward forever and the lane
    /// would go inert on exactly the boxes that need it.
    #[test]
    fn a_runaway_baseline_cannot_disable_the_lane() {
        let capped = machine_thread_shift(Some(AT_REST_BASELINE_MAX));
        for baseline in [AT_REST_BASELINE_MAX + 1, 10_000, usize::MAX] {
            assert_eq!(
                machine_thread_shift(Some(baseline)),
                capped,
                "the shift must stop growing past AT_REST_BASELINE_MAX"
            );
        }
        let merged = ceilings_for(Some(usize::MAX));
        assert_eq!(
            merged.warn,
            AT_REST_BASELINE_MAX + (SHIPPED_WARN_THREAD_CEILING - CALIBRATION_BASELINE)
        );
        assert_eq!(
            merged.critical,
            AT_REST_BASELINE_MAX + (SHIPPED_CRITICAL_THREAD_CEILING - CALIBRATION_BASELINE)
        );
        // The LIMIT is the quantity the cap moves; `o.observed` is the argument
        // echoed back and would assert nothing. 512 + 249 = 761.
        assert_eq!(merged.critical, 761);
        match evaluate_threads(Some(1_000), true, merged) {
            SpawnGate::Critical(o) => assert_eq!((o.observed, o.limit), (1_000, 761)),
            other => panic!("a leaking process must still be refused, got {other:?}"),
        }
    }

    /// The at-rest estimate subtracts the session-attributed threads at the
    /// instant of each reading, and yields NOTHING when either half is UNKNOWN
    /// or when the two halves are mutually incoherent.
    ///
    /// Pure arithmetic only — the process-global window is deliberately not
    /// touched here. [`AtRestWindow`] is tested on its own instance instead
    /// (see [`the_at_rest_window_can_rise_and_no_single_sample_can_pin_it`]),
    /// so no test in this binary asserts on shared mutable state.
    #[test]
    fn the_at_rest_estimate_subtracts_session_load_and_never_invents_one() {
        // 424 threads with 11 live sessions is the 2026-09-18 reading; on a
        // platform with no name census the constant path attributes 33.
        let attributed = session_thread_attribution(None, Some(11));
        assert_eq!(attributed, Some(33));
        assert_eq!(at_rest_estimate(Some(424), attributed), Some(391));

        // EITHER half UNKNOWN produces no sample at all. A `0` substituted for
        // the session count would raise the estimated floor, raise the ceiling
        // and loosen the guard exactly when the terminal registry is broken.
        assert_eq!(at_rest_estimate(None, Some(3)), None);
        assert_eq!(at_rest_estimate(Some(400), None), None);
        assert_eq!(at_rest_estimate(None, None), None);
        assert_eq!(session_thread_attribution(Some(328), None), None);
        assert_eq!(session_thread_attribution(None, None), None);
        assert_eq!(
            at_rest_estimate(Some(400), session_thread_attribution(Some(0), Some(0))),
            Some(400),
            "zero sessions is a READING, not an absence — it must still sample"
        );

        // An INCOHERENT reading is UNKNOWN, not a low floor. Saturating these
        // to `Some(0)` would fold a zero into the window as a legitimate floor
        // and drag the shift to zero for as long as it survived there — the
        // guard silently reverting to the latch, with nothing said.
        assert_eq!(at_rest_estimate(Some(1), Some(3000)), None);
        assert_eq!(
            at_rest_estimate(Some(10), session_thread_attribution(None, Some(usize::MAX))),
            None
        );
        // The boundary: session threads EQUAL to the whole process is still
        // incoherent — a live runner always carries unattributed threads.
        assert_eq!(at_rest_estimate(Some(9), Some(9)), None);
        assert_eq!(at_rest_estimate(Some(10), Some(9)), Some(1));
    }

    /// **Plan `2026-10-01-…-guard-dialog-says-low-memory`, Phase 1 — the
    /// incident's own census.** merytshost, 2026-10-01 15:04: 499 threads, no
    /// idle pool to grade out, 164 `terminal-reader` + 164 `terminal-waiter`,
    /// 164 live terminals. The baseline must read **171** — what the box
    /// actually idles at — and not `None` (the `164 × 3 = 492` constant path,
    /// which takes the INCOHERENT arm against any graded total the box carried
    /// and is what `origin/main` returned) or 7 (the draft's arithmetic over
    /// the raw count).
    ///
    /// Built through [`finish_thread_name_census`] from the RAW names each
    /// platform produces, so the collapse and the 12-row cap are exercised
    /// rather than bypassed: the Linux shape is `comm`-truncated, the Windows
    /// shape keeps `terminal-reader-<uuid>` and used to put every terminal on
    /// a row of its own.
    #[test]
    fn the_incident_census_yields_a_baseline_of_171() {
        use qontinui_runner_lib::wedge_diagnostics::finish_thread_name_census;
        let other = |names: &mut Vec<String>| {
            for i in 0..171 {
                names.push(format!("worker{}-x", i % 30));
            }
        };
        let mut linux = vec!["terminal-reader".to_string(); 164];
        linux.extend(vec!["terminal-waiter".to_string(); 164]);
        other(&mut linux);
        let mut windows: Vec<String> = Vec::new();
        for i in 0..164u32 {
            let id = format!("{i:08x}-1111-4222-8333-444455556666");
            windows.push(format!("terminal-reader-{id}"));
            windows.push(format!("terminal-waiter-{id}"));
        }
        other(&mut windows);

        for (platform, names) in [("linux", linux), ("windows", windows)] {
            let census = finish_thread_name_census(names).expect("census");
            assert_eq!(census.total, 499, "{platform}");
            // No idle pool in this census, so the graded reading is the raw one.
            let graded = graded_thread_reading(499, Some(&census), 0, RUNTIME_NAMES, 16);
            assert_eq!(graded.graded, 499, "{platform}");
            let attributed = session_thread_attribution(Some(census.session_threads), Some(164));
            assert_eq!(attributed, Some(328), "{platform}");
            assert_eq!(
                at_rest_estimate(Some(graded.graded), attributed),
                Some(171),
                "{platform}"
            );
            assert_eq!(
                per_session_threads_from(Some(census.session_threads), Some(164)),
                Some(2),
                "{platform}"
            );
        }

        // What the constant path made of the same box: 7 on the raw count, and
        // `None` on any graded total it actually carried — either way a zero
        // shift on the busiest box in the fleet.
        assert_eq!(
            at_rest_estimate(Some(499), session_thread_attribution(None, Some(164))),
            Some(7),
            "on the RAW count the constant path is the draft's 7…"
        );
        assert_eq!(
            at_rest_estimate(Some(300), session_thread_attribution(None, Some(164))),
            None,
            "…and against the graded 300 the guard quoted, it is UNKNOWN"
        );
    }

    /// **A leak cannot loosen the ceiling.** 20 live sessions and 300 leaked
    /// `terminal-reader` threads (340 named): the measured ratio would be 17
    /// per session, but no terminal holds more than the two family threads, so
    /// it is capped at [`MAX_THREADS_PER_SESSION`] — and even a raw 17 handed
    /// straight to the scaled term is capped there. The ceiling stays at the
    /// 2-per-session value.
    #[test]
    fn leaked_session_threads_do_not_raise_the_ceiling_past_two_per_session() {
        assert_eq!(MAX_THREADS_PER_SESSION, 2);
        assert_eq!(per_session_threads_from(Some(340), Some(20)), Some(2));

        let honest = ThreadCapacityInputs {
            per_session_threads: Some(2),
            session_threads_now: Some(340),
            ..merytshost_loaded()
        };
        let leaked = ThreadCapacityInputs {
            per_session_threads: Some(17),
            ..honest
        };
        let honest_scaled = scaled_thread_ceilings(&honest).unwrap();
        let leaked_scaled = scaled_thread_ceilings(&leaked).unwrap();
        assert_eq!(leaked_scaled.per_session_threads_used, 2);
        assert_eq!(leaked_scaled.ceilings, honest_scaled.ceilings);
        // …and that value is the 2-per-session session-capacity arm: 171 + 2 ×
        // (192 warn | 288 critical).
        assert!(leaked_scaled.ceilings.warn <= 171 + 2 * 192);
        assert!(leaked_scaled.ceilings.critical <= 171 + 2 * 288);
        let merged = merge_thread_ceilings(&defaults(), SessionFloors::default(), &leaked);
        assert!(merged.ceilings.warn <= 171 + 2 * 192, "{merged:?}");
    }

    /// When the census contradicts the session count, the scaled term is
    /// UNKNOWN and the floor is enforced — and provenance SAYS it stands on a
    /// mis-read rather than presenting it as an ordinary floor. Same numbers.
    #[test]
    fn a_census_misread_labels_the_floor_as_unknown_backed() {
        let misread = ThreadCapacityInputs {
            session_census_misread: true,
            ..merytshost_loaded()
        };
        assert_eq!(
            scaled_thread_ceilings(&misread),
            Err(ScaledUnknown::SessionCensusMisread)
        );
        let merged = merge_thread_ceilings(&defaults(), SessionFloors::default(), &misread);
        assert_eq!(
            merged.ceilings,
            ThreadCeilings {
                warn: 276,
                critical: 420
            }
        );
        assert_eq!(merged.warn_source, CeilingSource::CensusMisread);
        assert_eq!(merged.critical_source, CeilingSource::CensusMisread);
        let v = merged.to_json(true);
        assert_eq!(v["provenance"]["warn"], "census_misread");
        assert_eq!(v["scaledUnknown"], "session_census_misread");
        assert_eq!(v["inputs"]["sessionCensusMisread"], true);

        // An operator value is still the operator's — only a FLOOR is relabelled.
        let local = merge_thread_ceilings(
            &local_threads(Some(300), None),
            SessionFloors::default(),
            &misread,
        );
        assert_eq!(local.warn_source, CeilingSource::Local);
        assert_eq!(local.critical_source, CeilingSource::CensusMisread);
    }

    /// Both per-session families ABSENT while terminals are live is a mis-read,
    /// and a mis-read is UNKNOWN — never zero. Zero would attribute nothing,
    /// raise the baseline to the whole graded count and LOOSEN the guard.
    #[test]
    fn a_census_naming_no_session_threads_beside_live_terminals_is_unknown() {
        assert_eq!(session_thread_attribution(Some(0), Some(164)), None);
        // FEWER named threads than terminals is the same mis-read — typically
        // the 30 s memoized census lagging a fresh count after a spawn burst.
        assert_eq!(session_thread_attribution(Some(100), Some(164)), None);
        assert_eq!(session_thread_attribution(Some(164), Some(164)), Some(164));
        assert_eq!(
            at_rest_estimate(Some(499), session_thread_attribution(Some(0), Some(164))),
            None
        );
        // …but zero named threads with zero terminals is simply an idle box.
        assert_eq!(session_thread_attribution(Some(0), Some(0)), Some(0));
        // Named threads beside a registry reading of zero (a teardown race) are
        // still session threads by name; subtracting them is the strict side.
        assert_eq!(session_thread_attribution(Some(4), Some(0)), Some(4));

        // The per-session ratio has no UNKNOWN-as-zero arm either.
        assert_eq!(per_session_threads_from(Some(0), Some(164)), None);
        assert_eq!(
            per_session_threads_from(Some(100), Some(164)),
            None,
            "a mis-read"
        );
        assert_eq!(per_session_threads_from(Some(328), Some(0)), None);
        assert_eq!(per_session_threads_from(None, Some(164)), None);
        assert_eq!(
            per_session_threads_from(Some(329), Some(164)),
            Some(2),
            "rounds down"
        );
    }

    /// **The window must be able to RISE, and no single sample may pin it.**
    ///
    /// This is the property that separates a trailing window from the all-time
    /// minimum it replaced, and getting it wrong reinstates the latch. The
    /// tracked quantity rises over a process's life — 190 at boot, 424 after
    /// ~96 h on the measured box — so an all-time minimum degenerates to the
    /// FIRST sample, which is the boot figure, which is the defect.
    ///
    /// Asserted on a local [`AtRestWindow`], never the process-global one, so
    /// this test shares no mutable state with any other test in this binary.
    #[test]
    fn the_at_rest_window_can_rise_and_no_single_sample_can_pin_it() {
        let mut w = AtRestWindow::default();
        assert_eq!(w.baseline(), None, "no sample yet is UNKNOWN, not zero");

        // The real trajectory: boot low, then climb as the blocking pool
        // accumulates. An all-time minimum would answer 190 forever.
        for at_rest in [190, 240, 300, 402] {
            w.record(at_rest);
        }
        assert_eq!(
            w.baseline(),
            Some(190),
            "inside the window, the floor holds"
        );

        // Age the boot samples out. THIS is the assertion an all-time minimum
        // fails: after the window turns over, the baseline must have RISEN to
        // the floor the process actually has.
        for _ in 0..AT_REST_WINDOW_SAMPLES {
            w.record(402);
        }
        assert_eq!(
            w.baseline(),
            Some(402),
            "the baseline must rise as old samples age out, or the ladder stays \
             pinned to the boot floor and the latch returns"
        );
        assert_eq!(
            machine_thread_shift(w.baseline()),
            251,
            "402 - 151; the shift the 48-core box actually needs"
        );

        // A single spuriously low outlier lowers the floor — the SAFE
        // direction, the guard gets stricter — but only until it ages out.
        w.record(12);
        assert_eq!(w.baseline(), Some(12));
        for _ in 0..AT_REST_WINDOW_SAMPLES {
            w.record(402);
        }
        assert_eq!(
            w.baseline(),
            Some(402),
            "an outlier must age out; an all-time minimum would be pinned at 12 \
             for the life of the process, with no recovery short of a runner \
             restart that `runner-lifecycle` forbids"
        );

        // The window is BOUNDED — it is sampled every 30 s for the life of a
        // process that runs for days.
        assert_eq!(w.samples.len(), AT_REST_WINDOW_SAMPLES);
    }

    /// **A STALLED PUBLISHER DECAYS TO UNKNOWN, IT DOES NOT FREEZE.**
    ///
    /// `record_at_rest_sample` is reached only from
    /// `fleet::resource_sample::collect_host_lane`, and `publish_once` returns
    /// before `collect()` with no `machine.json`, no coord base, or no usable
    /// device JWT — the last of which is a routine transient, since device JWTs
    /// are short-lived.
    ///
    /// A count-bounded-only window would keep a floor recorded while the box
    /// was loaded for the LIFE OF THE PROCESS once the tick stopped, holding
    /// the ceilings shifted off data of unbounded age. That is the one arm of
    /// this change that would resolve PERMISSIVE, and it is the direction that
    /// matters: the guard whose job is catching a thread leak would be the one
    /// whose loosening outlived its own telemetry.
    #[test]
    fn a_stalled_publisher_decays_to_unknown_rather_than_freezing_a_stale_floor() {
        let t0 = std::time::Instant::now();
        let mut w = AtRestWindow::default();

        // A loaded box fills the window with a high floor, then the publisher
        // stops (no more `record_at` calls ever).
        for i in 0..AT_REST_WINDOW_SAMPLES {
            w.record_at(t0 + std::time::Duration::from_secs(i as u64), 402);
        }
        let last = t0 + std::time::Duration::from_secs(AT_REST_WINDOW_SAMPLES as u64);
        assert_eq!(w.baseline_at(last), Some(402), "fresh samples still count");

        // Inside the age bound the floor stands.
        assert_eq!(
            w.baseline_at(last + AT_REST_SAMPLE_MAX_AGE - std::time::Duration::from_secs(60)),
            Some(402),
        );

        // Past it, every sample is stale and the answer is UNKNOWN — which
        // `machine_thread_shift` renders as a ZERO shift, i.e. the shipped
        // 256/400. Strict, not permissive.
        let long_after = last + AT_REST_SAMPLE_MAX_AGE + std::time::Duration::from_secs(1);
        assert_eq!(
            w.baseline_at(long_after),
            None,
            "a window nothing has refreshed must read UNKNOWN, never a live floor"
        );
        assert_eq!(machine_thread_shift(w.baseline_at(long_after)), 0);
        let merged = ceilings_for(w.baseline_at(long_after));
        assert_eq!(merged.warn, SHIPPED_WARN_THREAD_CEILING);
        assert_eq!(merged.critical, SHIPPED_CRITICAL_THREAD_CEILING);

        // A clock that appears to go backwards reads as age zero rather than
        // underflowing, so a just-taken sample is never discarded.
        assert_eq!(
            w.baseline_at(t0 - std::time::Duration::from_secs(0)),
            Some(402)
        );
    }

    /// The calibration constants are the ones the shipped defaults were
    /// actually derived from — pinned so a future edit to either default has to
    /// come here and restate the relationship rather than silently changing
    /// what a ceiling MEANS.
    #[test]
    fn the_calibration_baseline_reproduces_the_shipped_headrooms() {
        assert_eq!(SHIPPED_WARN_THREAD_CEILING - CALIBRATION_BASELINE, 105);
        assert_eq!(SHIPPED_CRITICAL_THREAD_CEILING - CALIBRATION_BASELINE, 249);
        // …and those headrooms ARE the blocking-pool arm, unscaled.
        assert_eq!(POOL_HEADROOM_WARN, 105);
        assert_eq!(POOL_HEADROOM_CRITICAL, 249);
        assert!(
            CALIBRATION_BASELINE > crate::health_monitor::THREAD_WARNING_THRESHOLD,
            "the calibration baseline is a MEASURED idle count, not a threshold"
        );
        assert_eq!(
            AT_REST_BASELINE_MAX, 512,
            "tokio's per-runtime max_blocking_threads default — the anchor, not a round number"
        );
    }

    /// merytshost under the 2026-10-01 incident load: 48 cores, 368 GB, an
    /// at-rest floor of 171, 2 threads per terminal, 164 terminals (328
    /// session threads).
    fn merytshost_loaded() -> ThreadCapacityInputs {
        ThreadCapacityInputs {
            cores: Some(48),
            mem_total_bytes: Some(368_000_000_000),
            baseline: Some(171),
            per_session_threads: Some(2),
            session_threads_now: Some(328),
            session_census_misread: false,
        }
    }

    /// The same box carrying 10 sessions (20 session threads).
    fn merytshost_light() -> ThreadCapacityInputs {
        ThreadCapacityInputs {
            session_threads_now: Some(20),
            ..merytshost_loaded()
        }
    }

    /// **Plan `2026-10-01-…-guard-dialog-says-low-memory` §3, the worked values,
    /// as a table.** Each row: the inputs, the scaled pair, the enforced pair
    /// and the term that decided each.
    #[test]
    fn the_worked_values_scale_with_the_machine() {
        use CeilingSource::*;
        struct Row {
            name: &'static str,
            inputs: ThreadCapacityInputs,
            scaled: (usize, usize),
            enforced: (usize, usize),
            sources: (CeilingSource, CeilingSource),
        }
        let rows = [
            // cap_warn = min(4×48, ~902) = 192 → min(171 + 384, 171 + 328 + 105)
            // = min(555, 604); cap_crit = 288 → min(747, 748).
            Row {
                name: "merytshost, 164 sessions",
                inputs: merytshost_loaded(),
                scaled: (555, 747),
                enforced: (555, 747),
                sources: (Scaled, Scaled),
            },
            // The blocking-pool arm binds: a lightly loaded big box gets
            // today's pool headroom above its floor and nothing more.
            Row {
                name: "merytshost, 10 sessions",
                inputs: merytshost_light(),
                scaled: (296, 440),
                enforced: (296, 440),
                sources: (Scaled, Scaled),
            },
            // ≈ 29.8 GiB: memory binds at 63 sessions → 151 + 126 = 277 for
            // both; the critical is floored back to 400.
            Row {
                name: "16 cores / 32 GB, 63 sessions",
                inputs: ThreadCapacityInputs {
                    cores: Some(16),
                    mem_total_bytes: Some(32_000_000_000),
                    baseline: Some(151),
                    per_session_threads: Some(2),
                    session_threads_now: Some(126),
                    session_census_misread: false,
                },
                scaled: (277, 277),
                enforced: (277, 400),
                sources: (Scaled, Floor),
            },
            // Everything scaled sits under 256 / 400 and is floored back.
            Row {
                name: "4-core laptop / 16 GB",
                inputs: ThreadCapacityInputs {
                    cores: Some(4),
                    mem_total_bytes: Some(16_000_000_000),
                    baseline: Some(151),
                    per_session_threads: Some(2),
                    session_threads_now: Some(20),
                    session_census_misread: false,
                },
                scaled: (183, 191),
                enforced: (256, 400),
                sources: (Floor, Floor),
            },
        ];
        for row in rows {
            let scaled = scaled_thread_ceilings(&row.inputs).expect(row.name);
            assert_eq!(
                (scaled.ceilings.warn, scaled.ceilings.critical),
                row.scaled,
                "{}: {scaled:?}",
                row.name
            );
            let merged = merge_thread_ceilings(&defaults(), SessionFloors::default(), &row.inputs);
            assert_eq!(
                (merged.ceilings.warn, merged.ceilings.critical),
                row.enforced,
                "{}: {merged:?}",
                row.name
            );
            assert_eq!(
                (merged.warn_source, merged.critical_source),
                row.sources,
                "{}",
                row.name
            );
        }

        // Which arm bound, on the two merytshost rows.
        let loaded = scaled_thread_ceilings(&merytshost_loaded()).unwrap();
        assert_eq!(
            loaded.session_capacity,
            ThreadCeilings {
                warn: 192,
                critical: 288
            }
        );
        assert_eq!(
            loaded.ceilings, loaded.session_arm,
            "capacity binds under load"
        );
        let light = scaled_thread_ceilings(&merytshost_light()).unwrap();
        assert_eq!(
            light.ceilings, light.pool_arm,
            "the pool arm binds when light"
        );
    }

    /// Every UNKNOWN input that the scaled term cannot do without makes it
    /// `Err` with the reason named, and the enforced pair falls back to exactly
    /// `256 + shift` / `400 + shift` — the pre-scaling ceilings, never a
    /// permissive number. An UNKNOWN baseline or per-session figure is not one
    /// of those: they have documented stand-ins (151, 3).
    #[test]
    fn every_unknown_arm_falls_back_to_the_shifted_shipped_ceilings() {
        let shift = machine_thread_shift(Some(171));
        assert_eq!(shift, 20);
        let floor = ThreadCeilings {
            warn: 256 + shift,
            critical: 400 + shift,
        };
        for (inputs, why) in [
            (
                ThreadCapacityInputs {
                    cores: None,
                    ..merytshost_loaded()
                },
                ScaledUnknown::Cores,
            ),
            (
                ThreadCapacityInputs {
                    mem_total_bytes: None,
                    ..merytshost_loaded()
                },
                ScaledUnknown::MemTotal,
            ),
            (
                ThreadCapacityInputs {
                    session_threads_now: None,
                    ..merytshost_loaded()
                },
                ScaledUnknown::SessionThreadsNow,
            ),
        ] {
            assert_eq!(scaled_thread_ceilings(&inputs), Err(why));
            let merged = merge_thread_ceilings(&defaults(), SessionFloors::default(), &inputs);
            assert_eq!(merged.ceilings, floor, "{why:?}");
            assert_eq!(merged.warn_source, CeilingSource::Floor);
            assert_eq!(merged.scaled, Err(why));
        }

        // Nothing known at all: the shipped pair, byte for byte.
        let merged = merge_thread_ceilings(
            &defaults(),
            SessionFloors::default(),
            &ThreadCapacityInputs::default(),
        );
        assert_eq!(merged.ceilings, shipped());

        // The stand-ins, stated: an UNKNOWN baseline scales from 151, an
        // UNKNOWN per-session figure from 3.
        let stand_ins = scaled_thread_ceilings(&ThreadCapacityInputs {
            baseline: None,
            per_session_threads: None,
            ..merytshost_loaded()
        })
        .unwrap();
        assert_eq!(stand_ins.baseline_used, CALIBRATION_BASELINE);
        assert_eq!(
            stand_ins.per_session_threads_used,
            PER_SESSION_THREADS_FALLBACK
        );
        assert_eq!(
            PER_SESSION_THREADS_FALLBACK, 2,
            "the multiplier's fallback is min(3, families): the larger number would loosen"
        );
    }

    /// The operator's knob now works in BOTH directions; the fleet's still only
    /// tightens. D3 of the plan: a Settings value above the hardcoded term used
    /// to be saved and then silently discarded by the `min`.
    #[test]
    fn a_local_value_above_the_default_is_honoured_and_a_fleet_value_is_not() {
        let merged = fold(
            &local_threads(Some(1000), Some(1200)),
            SessionFloors::default(),
            None,
        );
        assert_eq!(
            merged.ceilings,
            ThreadCeilings {
                warn: 1000,
                critical: 1200
            }
        );
        assert_eq!(merged.warn_source, CeilingSource::Local);
        assert_eq!(merged.critical_source, CeilingSource::Local);

        let merged = fold(&defaults(), fleet_threads(Some(1000), Some(1200)), None);
        assert_eq!(
            merged.ceilings,
            shipped(),
            "a fleet row must never loosen a machine"
        );
        assert_eq!(merged.warn_source, CeilingSource::Floor);

        // …and a local value BELOW the scaled default still tightens it.
        let merged = merge_thread_ceilings(
            &local_threads(Some(300), Some(500)),
            SessionFloors::default(),
            &merytshost_loaded(),
        );
        assert_eq!(
            merged.ceilings,
            ThreadCeilings {
                warn: 300,
                critical: 500
            }
        );
    }

    /// Both clamps, at their ends. `clamp_min` is `THREAD_CEILING_MIN + shift`
    /// (vet correction 6): on a box idling at 400 an operator setting of 300
    /// would otherwise pin the ceiling under the process's own at-rest count —
    /// unspawnable forever. And no source reaches past
    /// [`THREAD_CEILING_ABS_MAX`], scaled term included.
    #[test]
    fn the_clamps_hold_at_both_ends() {
        let merged = fold(
            &local_threads(Some(300), Some(300)),
            SessionFloors::default(),
            Some(400),
        );
        assert_eq!(merged.clamp_min, THREAD_CEILING_MIN + 249);
        assert_eq!(
            merged.ceilings,
            ThreadCeilings {
                warn: 449,
                critical: 449
            }
        );
        assert_eq!(merged.warn_source, CeilingSource::ClampMin);
        assert_eq!(
            evaluate_threads(Some(400), true, merged.ceilings),
            SpawnGate::Proceed,
            "the box at its own at-rest floor must still spawn"
        );

        let merged = fold(
            &local_threads(Some(100_000), None),
            SessionFloors::default(),
            None,
        );
        assert_eq!(merged.ceilings.warn, THREAD_CEILING_ABS_MAX);
        assert_eq!(merged.warn_source, CeilingSource::ClampMax);
        // critical (the 400 floor) was below the clamped warn: the ladder.
        assert_eq!(merged.ceilings.critical, THREAD_CEILING_ABS_MAX);
        assert_eq!(merged.critical_source, CeilingSource::Ladder);

        let enormous = ThreadCapacityInputs {
            cores: Some(100_000),
            mem_total_bytes: Some(u64::MAX),
            baseline: Some(500),
            per_session_threads: Some(2),
            session_threads_now: Some(1_000_000),
            session_census_misread: false,
        };
        let merged = merge_thread_ceilings(&defaults(), SessionFloors::default(), &enormous);
        assert_eq!(
            merged.ceilings,
            ThreadCeilings {
                warn: THREAD_CEILING_ABS_MAX,
                critical: THREAD_CEILING_ABS_MAX
            }
        );
        assert_eq!(merged.warn_source, CeilingSource::ClampMax);
    }

    /// THE CLAMP, REWRITTEN. No combination of the three terms may compose a
    /// ceiling the runner is already over at rest — that is not a stricter
    /// guard, it is a machine that can never start a session again, on eight
    /// unattended seams with nobody to press "Start anyway".
    ///
    /// ## What this test used to assert, and why one half of it is overturned
    ///
    /// It also asserted `merged.warn <= defaults().warn + shift` (and the
    /// critical twin), labelled "loosened": no combination of local and fleet
    /// terms could walk a ceiling above the hardcoded default. That was a
    /// property of the `min` fold, and plan `2026-10-01-runner-thread-ceilings-
    /// ignore-the-machine-and-the-guard-dialog-says-low-memory` removes it FOR
    /// THE LOCAL TERM, deliberately. The hardcoded number was calibrated on one
    /// 151-thread idle runner; on the 48-core box that carried 164 sessions on
    /// 2026-10-01 it refused work the machine had capacity for, the refusal
    /// pointed the operator at Settings, and Settings could only make it
    /// stricter. The operator is the one party at the box; their stated number
    /// is now authoritative, bounded by the two clamps this test still pins.
    ///
    /// The property is NOT dropped for the FLEET term, and it is restated in a
    /// form that survives a machine-dependent default: a fleet row may never
    /// produce a looser pair than the same fold without it. A tenant-wide row
    /// loosening every box at once — the laptop included — is still the
    /// composition this test exists to forbid. And with no operator value at
    /// all, nothing loosens past the machine default.
    #[test]
    fn no_combination_of_terms_can_make_the_machine_unspawnable() {
        let locals = [
            None,
            Some(0),
            Some(1),
            Some(64),
            Some(151),
            Some(THREAD_CEILING_MIN),
            Some(256),
            Some(400),
            Some(1000),
            Some(usize::MAX),
        ];
        let fleets = [
            None,
            Some(0),
            Some(1),
            Some(64),
            Some(300),
            Some(1000),
            Some(u32::MAX),
        ];
        // Every shape of machine: nothing known, the 2026-09-18 48-core box
        // (floor 402), a capped runaway floor, and merytshost loaded and light.
        let machines = [
            machine(None),
            machine(Some(402)),
            machine(Some(usize::MAX)),
            merytshost_loaded(),
            merytshost_light(),
        ];
        for inputs in machines {
            let shift = machine_thread_shift(inputs.baseline);
            let clamp_min = THREAD_CEILING_MIN + shift;
            // What the process carries with zero sessions running.
            let at_rest = inputs
                .baseline
                .map_or(CALIBRATION_BASELINE, |b| b.min(AT_REST_BASELINE_MAX));
            let machine_default =
                merge_thread_ceilings(&defaults(), SessionFloors::default(), &inputs).ceilings;
            for lw in locals {
                for lc in locals {
                    let local = local_threads(lw, lc);
                    let without_fleet =
                        merge_thread_ceilings(&local, SessionFloors::default(), &inputs).ceilings;
                    for fw in fleets {
                        for fc in fleets {
                            let merged =
                                merge_thread_ceilings(&local, fleet_threads(fw, fc), &inputs);
                            let c = merged.ceilings;
                            let case = format!(
                                "local({lw:?},{lc:?}) fleet({fw:?},{fc:?}) {inputs:?} {merged:?}"
                            );
                            assert!(c.warn >= clamp_min, "unspawnable warn: {case}");
                            assert!(c.critical >= clamp_min, "unspawnable: {case}");
                            assert!(c.critical >= c.warn, "inverted: {case}");
                            assert!(c.critical <= THREAD_CEILING_ABS_MAX, "unbounded: {case}");
                            // The FLEET term never loosens.
                            assert!(
                                c.warn <= without_fleet.warn
                                    && c.critical <= without_fleet.critical,
                                "the fleet loosened: {case}"
                            );
                            // With no operator value, nothing passes the
                            // machine default.
                            if lw.is_none() && lc.is_none() {
                                assert!(
                                    c.warn <= machine_default.warn
                                        && c.critical <= machine_default.critical,
                                    "loosened past the machine default: {case}"
                                );
                            }
                            // The LOCAL term may loosen: an in-range operator
                            // warn ceiling with no tighter fleet term is
                            // enforced verbatim, above the default or below.
                            if let Some(v) = lw {
                                if (clamp_min..=THREAD_CEILING_ABS_MAX).contains(&v)
                                    && fw.is_none_or(|f| f as usize >= v)
                                {
                                    assert_eq!(c.warn, v, "operator value not honoured: {case}");
                                }
                            }
                            // The runner at its own at-rest count still spawns
                            // under EVERY composable configuration — the
                            // property the clamp exists to buy.
                            assert_eq!(
                                evaluate_threads(Some(at_rest), true, c),
                                SpawnGate::Proceed,
                                "an idle runner must never be refused: {case}"
                            );
                        }
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
        const { assert!(SHIPPED_WARN_THREAD_CEILING > crate::health_monitor::THREAD_WARNING_THRESHOLD) };

        // The clamp sits strictly below both shipped ceilings, or it leaves the
        // operator no room to tighten under the machine default.
        const { assert!(THREAD_CEILING_MIN < SHIPPED_WARN_THREAD_CEILING) };
        const { assert!(THREAD_CEILING_MIN < SHIPPED_CRITICAL_THREAD_CEILING) };

        // A usable warn band, or the warn verdict could never fire.
        const { assert!(SHIPPED_CRITICAL_THREAD_CEILING > SHIPPED_WARN_THREAD_CEILING) };

        // Both ceilings are fractions of tokio's 512-slot blocking pool
        // (unreconfigured in every production runtime in this binary): half of
        // it warns, and the critical ceiling leaves ~112 slots for the race
        // between reading the count and the PTY actually opening.
        assert_eq!(SHIPPED_WARN_THREAD_CEILING, 512 / 2);
        const { assert!(SHIPPED_CRITICAL_THREAD_CEILING <= 512 - 100) };

        // The upper clamp: four pools, past which a ceiling could not fire
        // before the pool it protects was exhausted.
        assert_eq!(THREAD_CEILING_ABS_MAX, 4 * 512);
        // The largest lower clamp a machine can reach stays far under it.
        assert!(
            THREAD_CEILING_MIN + machine_thread_shift(Some(usize::MAX)) < THREAD_CEILING_ABS_MAX
        );

        // The measured capacity constants (merytshost, 2026-10-01).
        assert_eq!(SESSIONS_PER_CORE_WARN, 4);
        assert_eq!(SESSIONS_PER_CORE_CRITICAL, 6);
        assert_eq!(PER_SESSION_RSS_BYTES, 350 * 1024 * 1024);
    }

    /// THE CEILING LADDER'S OWN CASE. A tenant that has just watched a machine
    /// wedge sets ONLY the critical ceiling — 120 — and leaves the warn ceiling
    /// NULL. Two clamps fire in order, and both are needed:
    ///
    /// 1. [`fold_ceiling`] raises 120 to the lower clamp (200 here), because a
    ///    live runner idles at 150-151 and a 120-thread ceiling is a machine
    ///    that can never start a session again.
    /// 2. That still leaves critical 200 BELOW the warn ceiling 256, and since
    ///    [`evaluate_threads`] tests critical first, every reading over 200
    ///    would be a refusal with no warn band left at all. The ladder coercion
    ///    raises critical to the warn ceiling.
    #[test]
    fn a_fleet_critical_ceiling_with_a_null_warn_column_cannot_invert_the_ladder() {
        let merged = fold(&defaults(), fleet_threads(None, Some(120)), None);

        // The warn ceiling is NOT lowered to meet the critical one: that would
        // enforce a ceiling nobody stated, below the measured at-rest band,
        // warning on every spawn forever.
        assert_eq!(merged.ceilings.warn, SHIPPED_WARN_THREAD_CEILING);
        // The critical ceiling is raised to it — the weakest correction.
        assert_eq!(merged.ceilings.critical, merged.ceilings.warn);
        assert_eq!(merged.critical_source, CeilingSource::Ladder);
        assert_eq!(
            merged.coercion,
            Some(LadderCoercion {
                metric: LaneMetric::ThreadCount,
                // Post-clamp: the fleet's 120 was already raised to 200 by
                // `fold_ceiling` before the ladder saw it — the same
                // composition order the floor lane uses (clamp, then ladder).
                requested_critical: THREAD_CEILING_MIN as u64,
                warn: SHIPPED_WARN_THREAD_CEILING as u64,
            })
        );

        // And the machine is still spawnable at its measured idle count.
        assert_eq!(
            evaluate_threads(Some(151), true, merged.ceilings),
            SpawnGate::Proceed
        );
    }

    /// …and the case where the clamp CANNOT hide it: a machine owner who
    /// set their warn ceiling to 220, plus a tenant who states a critical
    /// ceiling of 210. Neither party wrote an inverted ladder; the fold
    /// composed one, both terms are above the clamp, and the ladder coercion
    /// is the only thing left to fix it.
    #[test]
    fn crossed_terms_above_the_floor_constant_still_invert_and_are_coerced() {
        let merged = fold(
            &local_threads(Some(220), None),
            fleet_threads(None, Some(210)),
            None,
        );

        // The warn ceiling is NOT dragged down to 210: that would enforce a
        // limit neither party stated, tightening past both inputs.
        assert_eq!(merged.ceilings.warn, 220);
        // The critical ceiling is raised to it — the weakest correction.
        assert_eq!(merged.ceilings.critical, 220);
        assert_eq!(
            merged.coercion,
            Some(LadderCoercion {
                metric: LaneMetric::ThreadCount,
                requested_critical: 210,
                warn: 220,
            })
        );
    }

    /// The ladder coercion is visible when the clamps do not already hide it:
    /// a local pair that is inverted well above the clamp. (The save command
    /// refuses such a pair; a hand-edited `settings.json` can still hold one.)
    #[test]
    fn an_inverted_local_ceiling_pair_is_corrected_the_weak_way() {
        let merged = fold(
            &local_threads(Some(240), Some(210)),
            SessionFloors::default(),
            None,
        );
        assert_eq!(merged.ceilings.warn, 240, "warn is never dragged down");
        assert_eq!(merged.ceilings.critical, 240, "critical is raised to it");
        assert_eq!(
            merged.coercion,
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
        let merged = fold(
            &local_threads(Some(240), Some(240)),
            SessionFloors::default(),
            None,
        );
        assert_eq!(
            merged.ceilings,
            ThreadCeilings {
                warn: 240,
                critical: 240
            }
        );
        assert_eq!(merged.coercion, None);
        assert_eq!(merged.critical_source, CeilingSource::Local);
    }

    /// A fleet ceiling can change the VERDICT, not just the number — the same
    /// end-to-end property the floor lane's fleet term has, on the day coord
    /// starts publishing the column.
    #[test]
    fn a_fleet_ceiling_can_change_the_verdict() {
        let reading = Some(320);

        // Machine default alone: 320 is between 256 and 400, so it warns.
        let local_only = fold(&defaults(), SessionFloors::default(), None);
        assert!(matches!(
            evaluate_threads(reading, true, local_only.ceilings),
            SpawnGate::Warn(_)
        ));

        // The tenant declares a 300-thread critical ceiling; the same reading
        // is now a refusal.
        let merged = fold(&defaults(), fleet_threads(None, Some(300)), None);
        assert_eq!(
            evaluate_threads(reading, true, merged.ceilings),
            SpawnGate::Critical(thread_observation(320, 300))
        );
    }

    /// Each fold authors ITS OWN lane and leaves the other lane alone. The
    /// floor fold copies the operator's thread overrides through untouched
    /// (they are not limits until the thread fold reads them), and the thread
    /// fold produces ceilings and nothing else — a thread ceiling silently
    /// reset by a memory fold is a limit that stops enforcing on the spawn path
    /// with nothing logged.
    #[test]
    fn each_fold_leaves_the_other_lanes_fields_alone() {
        let local = SessionGuardSettings {
            warn_free_commit_bytes: 8 * GIB_U64,
            critical_free_commit_bytes: 4 * GIB_U64,
            warn_thread_count: Some(200),
            critical_thread_count: Some(300),
            enabled: true,
        };

        let floors = merge_floors(&local, fleet_bytes(Some(9 * GIB_U64), None));
        assert_eq!(floors.warn_free_commit_bytes, 9 * GIB_U64);
        assert_eq!(floors.warn_thread_count, Some(200));
        assert_eq!(floors.critical_thread_count, Some(300));

        let ceilings = fold(&local, fleet_threads(None, Some(250)), None);
        assert_eq!(ceilings.ceilings.critical, 250);
        // The owner's warn ceiling of 200 survives: the fleet states no warn
        // term and 200 is at, not under, the clamp.
        assert_eq!(ceilings.ceilings.warn, 200);
        assert_eq!(ceilings.local, (Some(200), Some(300)));
    }

    /// The provenance report `/health` `threadCeilings` and the Settings panel
    /// both render: every number with the term that decided it, every UNKNOWN
    /// as `null` with its reason named.
    #[test]
    fn the_thread_ceilings_report_names_every_number_and_its_source() {
        let merged =
            merge_thread_ceilings(&defaults(), SessionFloors::default(), &merytshost_loaded());
        let v = merged.to_json(true);
        assert_eq!(v["enabled"], true);
        assert_eq!(v["warn"], 555);
        assert_eq!(v["critical"], 747);
        assert_eq!(v["provenance"]["warn"], "scaled");
        assert_eq!(v["provenance"]["critical"], "scaled");
        assert_eq!(v["floor"]["warn"], 276);
        assert_eq!(v["shift"], 20);
        assert_eq!(v["clampMin"], 220);
        assert_eq!(v["absMax"], THREAD_CEILING_ABS_MAX);
        assert_eq!(v["scaled"]["sessionArm"]["warn"], 555);
        assert_eq!(v["scaled"]["poolArm"]["warn"], 604);
        assert_eq!(v["scaled"]["sessionCapacity"]["critical"], 288);
        assert!(v["scaledUnknown"].is_null());
        assert_eq!(v["inputs"]["cores"], 48);
        assert_eq!(v["inputs"]["sessionThreadsNow"], 328);
        assert!(
            v["local"]["warn"].is_null(),
            "unset is null, never a number"
        );
        assert!(v["fleet"]["critical"].is_null());
        assert_eq!(v["ladderCoerced"], false);

        let unknown = merge_thread_ceilings(
            &defaults(),
            SessionFloors::default(),
            &ThreadCapacityInputs::default(),
        )
        .to_json(false);
        assert!(unknown["scaled"].is_null());
        assert_eq!(unknown["scaledUnknown"], "cores_unknown");
        assert!(unknown["inputs"]["baseline"].is_null());
        assert_eq!(unknown["provenance"]["warn"], "floor");
        assert_eq!(unknown["enabled"], false);
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
        let threads = evaluate_threads(Some(540), guard.enabled, shipped());
        let (reported, shadowed) = compose_lanes(memory, threads);
        match &reported {
            SpawnGate::Critical(o) => {
                assert_eq!(o.lane, Lane::Threads.as_str());
                assert_eq!(o.observed, 540);
                assert_eq!(o.limit, shipped().critical as u64);
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
        let guard = fold(&defaults(), SessionFloors::default(), None).ceilings;

        // A continuation-deferring reading: over the warn ceiling, under the
        // critical one. `admit_spawn` would still let a human's terminal start.
        assert!(matches!(
            evaluate_threads(Some(300), true, guard),
            SpawnGate::Warn(_)
        ));
        // A spawn-refusing reading.
        assert!(matches!(
            evaluate_threads(Some(450), true, guard),
            SpawnGate::Critical(_)
        ));
        // And the band is non-empty, or the distinction would be unreachable.
        assert!(guard.critical > guard.warn + 1);
    }

    // -----------------------------------------------------------------------
    // Background-work shedding (plan 2026-09-23-…-ungated, Phase 3)
    // -----------------------------------------------------------------------

    /// The verdict table over (reading, floors). Every row is the reading
    /// against the shipped defaults (3 GiB warn, 1.5 GiB critical) unless it
    /// names otherwise.
    #[test]
    fn background_work_verdict_table() {
        let d = defaults();
        let warn = d.warn_free_commit_bytes;
        let crit = d.critical_free_commit_bytes;
        let off = SessionGuardSettings {
            enabled: false,
            ..defaults()
        };
        let tightened = SessionGuardSettings {
            warn_free_commit_bytes: 8 * GIB_U64,
            critical_free_commit_bytes: 6 * GIB_U64,
            ..defaults()
        };
        #[derive(Debug, PartialEq)]
        enum V {
            Run,
            Throttle,
            Skip,
        }
        let rows: Vec<(&str, Option<u64>, &SessionGuardSettings, V)> = vec![
            // UNKNOWN reading ⇒ run exactly as today (fail open).
            ("unknown reading", None, &d, V::Run),
            // Disabled guard ⇒ run, at every reading including zero.
            ("disabled, zero free", Some(0), &off, V::Run),
            ("disabled, unknown", None, &off, V::Run),
            ("plenty", Some(32 * GIB_U64), &d, V::Run),
            // Strictly below: exactly at a floor is not under it.
            ("at warn floor", Some(warn), &d, V::Run),
            ("one byte under warn", Some(warn - 1), &d, V::Throttle),
            ("at critical floor", Some(crit), &d, V::Throttle),
            ("one byte under critical", Some(crit - 1), &d, V::Skip),
            ("zero free", Some(0), &d, V::Skip),
            // A machine owner who tightened the floors sheds earlier — the
            // verdict reads the effective floors, not a constant of its own.
            (
                "5 GiB on a tightened box",
                Some(5 * GIB_U64),
                &tightened,
                V::Skip,
            ),
            (
                "7 GiB on a tightened box",
                Some(7 * GIB_U64),
                &tightened,
                V::Throttle,
            ),
        ];
        for (name, reading, floors, want) in rows {
            let got = match background_work_verdict_for("host", reading, floors) {
                BackgroundWork::Run => V::Run,
                BackgroundWork::Throttle(o) => {
                    assert_eq!(o.limit, floors.warn_free_commit_bytes, "{name}");
                    V::Throttle
                }
                BackgroundWork::Skip(o) => {
                    assert_eq!(o.limit, floors.critical_free_commit_bytes, "{name}");
                    V::Skip
                }
            };
            assert_eq!(got, want, "{name}");
        }
    }

    /// A `SkipTick` spender skips at Throttle and at Skip and resumes on the
    /// very next Run — no backoff.
    #[test]
    fn skip_tick_policy_resumes_immediately() {
        let mut shed = BackgroundShed::new("test_skip_tick", ShedPolicy::SkipTick);
        assert!(shed.admit(&BackgroundWork::Run));
        for _ in 0..10 {
            assert!(!shed.admit(&test_throttle_verdict()));
        }
        assert!(!shed.admit(&test_skip_verdict()));
        assert!(!shed.admit(&test_skip_verdict()));
        assert!(shed.admit(&BackgroundWork::Run));
    }

    /// `ThrottleAndBackoff`: the hold-off doubles per consecutive critical
    /// tick, caps at [`SHED_BACKOFF_MAX_CYCLES`], and is spent by Run ticks.
    #[test]
    fn backoff_holds_off_exponentially_and_caps() {
        let held_off_after = |critical_ticks: u32| {
            let mut shed = BackgroundShed::new("test_backoff", ShedPolicy::ThrottleAndBackoff);
            for _ in 0..critical_ticks {
                assert!(!shed.admit(&test_skip_verdict()));
            }
            let mut skipped = 0;
            while !shed.admit(&BackgroundWork::Run) {
                skipped += 1;
                assert!(skipped <= SHED_BACKOFF_MAX_CYCLES, "backoff must be capped");
            }
            skipped
        };
        assert_eq!(held_off_after(1), 1);
        assert_eq!(held_off_after(2), 2);
        assert_eq!(held_off_after(3), 4);
        assert_eq!(held_off_after(40), SHED_BACKOFF_MAX_CYCLES);
    }

    /// Sustained WARN reduces the rate, it does not stop the work: exactly one
    /// tick in [`SHED_THROTTLE_RUN_EVERY`] runs, forever, and the first Run
    /// verdict resumes every tick.
    #[test]
    fn sustained_throttle_runs_one_tick_in_four() {
        let mut shed = BackgroundShed::new("test_throttle", ShedPolicy::ThrottleAndBackoff);
        let ran: Vec<bool> = (0..12)
            .map(|_| shed.admit(&test_throttle_verdict()))
            .collect();
        let expected: Vec<bool> = (1..=12).map(|i| i % SHED_THROTTLE_RUN_EVERY == 0).collect();
        assert_eq!(ran, expected);
        assert!(shed.admit(&BackgroundWork::Run));
        assert!(shed.admit(&BackgroundWork::Run));
    }

    /// A critical dip followed by a long WARN plateau comes back to the reduced
    /// rate: Throttle ticks spend the hold-off, then one in four runs.
    #[test]
    fn critical_then_sustained_throttle_recovers_to_the_reduced_rate() {
        let mut shed = BackgroundShed::new("test_dip", ShedPolicy::ThrottleAndBackoff);
        for _ in 0..3 {
            assert!(!shed.admit(&test_skip_verdict()));
        }
        let ran = (0..40)
            .filter(|_| shed.admit(&test_throttle_verdict()))
            .count();
        // 4 ticks of hold-off, then 36 throttled ticks of which 9 run.
        assert_eq!(ran, 9);
    }

    /// N-1: the hold-off line never claims pressure eased while the verdict is
    /// still WARN — it quotes the warn clause instead.
    #[test]
    fn holding_off_line_is_honest_under_warn() {
        let ((), logs) = capture_logs(|| {
            let mut shed = BackgroundShed::new("dip_spender", ShedPolicy::ThrottleAndBackoff);
            shed.admit(&test_skip_verdict());
            shed.admit(&test_throttle_verdict());
        });
        let line = logs
            .lines()
            .find(|l| l.contains("holding off"))
            .expect("a hold-off line");
        assert!(!line.contains("eased"), "{line}");
        assert!(line.contains("below the 3.00 GiB warn floor"), "{line}");
    }

    /// The edge trigger: a spender that stays shed logs ONE line on entry and
    /// one on recovery, never one per tick; a spender that never sheds logs
    /// nothing at all.
    #[test]
    fn shed_log_is_edge_triggered() {
        let ((), logs) = capture_logs(|| {
            let mut shed = BackgroundShed::new("edge_spender", ShedPolicy::SkipTick);
            for _ in 0..3 {
                shed.admit(&BackgroundWork::Run);
            }
            for _ in 0..50 {
                shed.admit(&test_skip_verdict());
            }
            for _ in 0..3 {
                shed.admit(&BackgroundWork::Run);
            }
        });
        assert_eq!(
            logs.matches("skipping edge_spender's background work")
                .count(),
            1,
            "{logs}"
        );
        assert_eq!(logs.matches("edge_spender resumed").count(), 1, "{logs}");
        assert_eq!(logs.lines().count(), 2, "{logs}");
        // The skip line quotes the reading and the floor it fell below.
        assert!(logs.contains("below the 1.50 GiB critical floor"), "{logs}");

        let ((), quiet) = capture_logs(|| {
            let mut shed = BackgroundShed::new("quiet_spender", ShedPolicy::ThrottleAndBackoff);
            for _ in 0..10 {
                shed.admit(&BackgroundWork::Run);
            }
        });
        assert!(quiet.is_empty(), "{quiet}");
    }
}
