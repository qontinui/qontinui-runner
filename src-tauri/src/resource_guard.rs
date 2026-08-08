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
//! ## Fail OPEN, always
//!
//! `commit_available_bytes()` returns `Option`. `None` means the sensor is
//! UNKNOWN, UNKNOWN means this gate has no opinion, and no opinion means
//! **proceed** — see [`SpawnGate::Proceed`]. Every other guard in this fleet's
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

use std::sync::Mutex;

use tauri::{AppHandle, Emitter};
use tracing::warn;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnGate {
    /// Enough headroom — or no readable opinion at all. Spawn.
    Proceed,
    /// Below the warn floor. Spawn anyway, but tell the operator: the point of
    /// the warning is that they can free memory *before* the next spawn, and
    /// blocking here would be a heavier verdict than the evidence supports.
    Warn {
        lane: String,
        free_bytes: u64,
        floor_bytes: u64,
    },
    /// Below the critical floor. Refuse by default, and let an explicit
    /// override through — a false positive here blocks the operator's actual
    /// work, which is a worse failure than an occasional missed warning.
    Critical {
        lane: String,
        free_bytes: u64,
        floor_bytes: u64,
    },
}

/// Pure verdict over an injected reading and the machine owner's floors.
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
    if free < guard.critical_free_commit_bytes {
        return SpawnGate::Critical {
            lane: lane.to_string(),
            free_bytes: free,
            floor_bytes: guard.critical_free_commit_bytes,
        };
    }
    if free < guard.warn_free_commit_bytes {
        return SpawnGate::Warn {
            lane: lane.to_string(),
            free_bytes: free,
            floor_bytes: guard.warn_free_commit_bytes,
        };
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
            enabled: local.enabled,
        },
        coercion,
    )
}

/// A `critical > warn` ladder the three-term fold produced, and what it was
/// clamped to. Reported by [`merge_floors_reporting`], logged (once, on a
/// transition) by [`effective_session_floors`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LadderCoercion {
    /// The critical floor the fold produced, before clamping.
    pub(crate) requested_critical_bytes: u64,
    /// The warn floor it was clamped down to.
    pub(crate) warn_bytes: u64,
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
            requested_critical_bytes: critical,
            warn_bytes: warn,
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

/// The last ladder coercion logged, so the line is emitted on a TRANSITION and
/// not on every spawn.
static LAST_COERCION: Mutex<Option<(String, LadderCoercion)>> = Mutex::new(None);

/// Log a [`LadderCoercion`] once, edge-triggered.
///
/// [`effective_session_floors`] runs on every spawn and on every `ci_node`
/// headroom probe, so an unconditional line would put the same warning in the
/// log dozens of times an hour while telling the operator nothing new. This is
/// the same discipline `mcp::fleet_policy_poller`'s loop uses for its own
/// degradations: remember the last state logged, emit only when it changes. A
/// coercion that STOPS (the tenant fixed the row, or the operator raised their
/// warn floor) clears the marker, so if it ever comes back it is reported again
/// rather than swallowed as "already said that".
///
/// The lane is part of the remembered state because the floors are
/// lane-separated: a host-lane inversion and a WSL-lane inversion are two
/// different misconfigurations and both deserve their own line.
fn note_ladder_coercion(lane: &str, coercion: Option<LadderCoercion>) {
    let mut last = LAST_COERCION.lock().unwrap_or_else(|e| e.into_inner());
    let now = coercion.map(|c| (lane.to_string(), c));
    if *last == now {
        return;
    }
    *last = now;
    if let Some(c) = coercion {
        warn!(
            lane = %lane,
            requested_critical_bytes = c.requested_critical_bytes,
            warn_bytes = c.warn_bytes,
            "resource_guard: the effective {lane} critical floor ({}) was above the warn floor \
             ({}) — clamping it to the warn floor. Left as folded, every reading below the \
             critical floor would be a refusal and nothing would ever warn. Check the tenant's \
             fleet-policy row: setting only the critical column leaves the warn column on the \
             hardcoded default.",
            format_gib(c.requested_critical_bytes),
            format_gib(c.warn_bytes),
        );
    }
}

/// Live verdict: read the floors, take one host-lane reading, [`evaluate`].
///
/// The settings read happens FIRST and short-circuits when the guard is
/// disabled, so a machine owner who turned the guard off pays nothing at all —
/// not even the one `GlobalMemoryStatusEx` call — on every spawn. [`evaluate`]
/// also honours `enabled` so the pure function is complete on its own; the check
/// here is about cost, not correctness.
///
/// The reading is [`crate::fleet::resource_sample::spawn_gate_reading`] — the
/// lane name and the free-commit figure, and nothing else. It is deliberately
/// NOT the publisher's full host-lane sample: that one enumerates every volume
/// on the box, reads settings and computes build occupancy, none of which this
/// verdict consults, and this function runs synchronously on a tokio worker
/// under every unattended spawn seam. See this module's "Host lane only" section
/// for the full argument. Both paths read free commit through the same
/// `available_commit_bytes()`, so the gate and the fleet dashboard still agree on
/// the quantity.
///
/// The fleet term is folded in AFTER the reading, because the lane to look the
/// fleet floors up under comes from the reading itself.
pub(crate) fn probe_for_spawn() -> SpawnGate {
    let local = crate::settings::get_session_guard_settings();
    if !local.enabled {
        return SpawnGate::Proceed;
    }
    let (lane, free_commit_bytes) = crate::fleet::resource_sample::spawn_gate_reading();
    let floors = effective_session_floors(&local, lane);
    evaluate(lane, free_commit_bytes, &floors)
}

/// `bytes` as `"1.42 GiB"`. Two decimals because the shipped critical floor is
/// 1.5 GiB and rounding it to `2 GiB` in the very message that quotes it would
/// misreport the configured value.
fn format_gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB)
}

/// The refusal text a CRITICAL verdict returns, prefixed for machine matching.
///
/// Names the lane, the current headroom and the configured floor, because a
/// refusal that says only "not enough memory" gives the operator nothing to act
/// on — they cannot tell whether to close a build, close a session, or raise a
/// floor that was set too high.
fn critical_refusal(what: &str, lane: &str, free_bytes: u64, floor_bytes: u64) -> String {
    format!(
        "{CRITICAL_REFUSAL_PREFIX} Not starting a new {what}: the {lane} lane has \
         {} of free commit, below the {} critical floor. Free memory (close a build \
         or a session) and try again, or start anyway to override. The floors live \
         in Settings > Resource Guard.",
        format_gib(free_bytes),
        format_gib(floor_bytes),
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
pub(crate) fn admit_spawn(
    what: &str,
    resource_override: bool,
    app: Option<&AppHandle>,
) -> Result<(), String> {
    match probe_for_spawn() {
        SpawnGate::Proceed => Ok(()),
        SpawnGate::Warn {
            lane,
            free_bytes,
            floor_bytes,
        } => {
            let message = format!(
                "Low memory: the {lane} lane has {} of free commit, below the {} warn \
                 floor. Starting this {what} anyway.",
                format_gib(free_bytes),
                format_gib(floor_bytes),
            );
            warn!(
                lane = %lane,
                free_bytes,
                floor_bytes,
                what = %what,
                "resource_guard: spawning below the session warn floor"
            );
            emit_notice(app, "warn", &lane, free_bytes, floor_bytes, &message);
            Ok(())
        }
        SpawnGate::Critical {
            lane,
            free_bytes,
            floor_bytes,
        } => {
            if resource_override {
                let message = format!(
                    "Started this {what} below the {} critical floor ({} free on the \
                     {lane} lane) — the resource guard was overridden.",
                    format_gib(floor_bytes),
                    format_gib(free_bytes),
                );
                warn!(
                    lane = %lane,
                    free_bytes,
                    floor_bytes,
                    what = %what,
                    "resource_guard: OVERRIDDEN — spawning below the session critical floor"
                );
                emit_notice(app, "override", &lane, free_bytes, floor_bytes, &message);
                return Ok(());
            }
            warn!(
                lane = %lane,
                free_bytes,
                floor_bytes,
                what = %what,
                "resource_guard: refusing to spawn below the session critical floor"
            );
            Err(critical_refusal(what, &lane, free_bytes, floor_bytes))
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
/// `GlobalMemoryStatusEx` call and leaks nothing.
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
        SpawnGate::Critical {
            lane,
            free_bytes,
            floor_bytes,
        } => {
            warn!(
                lane = %lane,
                free_bytes,
                floor_bytes,
                what = %what,
                "resource_guard: refusing a {what} before its worktree/claim acquisition \
                 (pre-check; the spawn seam would refuse it too)"
            );
            Err(critical_refusal(what, &lane, free_bytes, floor_bytes))
        }
        SpawnGate::Proceed | SpawnGate::Warn { .. } => Ok(()),
    }
}

/// Best-effort webview notice. A failed emit is logged and swallowed: a toast
/// that could not be delivered must never turn into a spawn failure.
fn emit_notice(
    app: Option<&AppHandle>,
    severity: &str,
    lane: &str,
    free_bytes: u64,
    floor_bytes: u64,
    message: &str,
) {
    let Some(app) = app else {
        return;
    };
    if let Err(e) = app.emit(
        RESOURCE_GUARD_EVENT,
        serde_json::json!({
            "severity": severity,
            "lane": lane,
            "freeBytes": free_bytes,
            "floorBytes": floor_bytes,
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

    /// The shipped defaults: 3 GiB warn, 1.5 GiB critical, enabled.
    fn defaults() -> SessionGuardSettings {
        SessionGuardSettings::default()
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
            SpawnGate::Warn {
                lane: "host".to_string(),
                free_bytes: 2 * GIB_U64,
                floor_bytes: g.warn_free_commit_bytes,
            }
        );
    }

    #[test]
    fn below_the_critical_floor_is_critical() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(GIB_U64), &g),
            SpawnGate::Critical {
                lane: "host".to_string(),
                free_bytes: GIB_U64,
                floor_bytes: g.critical_free_commit_bytes,
            }
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
            SpawnGate::Warn { free_bytes, .. } => {
                assert_eq!(free_bytes, g.critical_free_commit_bytes)
            }
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
            enabled: true,
        };
        match evaluate("host", Some(2 * GIB_U64), &inverted) {
            SpawnGate::Critical { floor_bytes, .. } => assert_eq!(floor_bytes, 4 * GIB_U64),
            other => panic!("expected Critical under transposed floors, got {other:?}"),
        }
    }

    /// The lane is carried through from the reading rather than hardcoded, so
    /// the message names the lane that was actually measured.
    #[test]
    fn lane_is_carried_from_the_reading() {
        match evaluate("wsl", Some(0), &defaults()) {
            SpawnGate::Critical { lane, .. } => assert_eq!(lane, "wsl"),
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
        let msg = critical_refusal("terminal session", "host", 1_073_741_824, 1_610_612_736);
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
            enabled: true,
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
            SessionFloors {
                warn_free_bytes: Some(6 * GIB_U64),
                critical_free_bytes: Some(3 * GIB_U64),
            },
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
            enabled: true,
        };
        let merged = merge_floors(
            &tightened,
            SessionFloors {
                warn_free_bytes: Some(4 * GIB_U64),
                critical_free_bytes: Some(GIB_U64),
            },
        );
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
            enabled: true,
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
        let with_fleet_zero = merge_floors(
            &loosened,
            SessionFloors {
                warn_free_bytes: Some(0),
                critical_free_bytes: Some(0),
            },
        );
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
        let (merged, coercion) = merge_floors_reporting(
            &defaults(),
            SessionFloors {
                warn_free_bytes: Some(9 * GIB_U64),
                critical_free_bytes: None,
            },
        );
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
        let (merged, coercion) = merge_floors_reporting(
            &defaults(),
            SessionFloors {
                warn_free_bytes: None,
                critical_free_bytes: Some(6 * GIB_U64),
            },
        );

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
                requested_critical_bytes: 6 * GIB_U64,
                warn_bytes: 3 * GIB_U64,
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
            enabled: true,
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
                enabled: true,
            },
            SessionFloors {
                warn_free_bytes: None,
                critical_free_bytes: Some(7 * GIB_U64),
            },
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
            enabled: true,
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
                                enabled: true,
                            },
                            SessionFloors {
                                warn_free_bytes: fw,
                                critical_free_bytes: fc,
                            },
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
        let merged = merge_floors(
            &defaults(),
            SessionFloors {
                warn_free_bytes: Some(u64::MAX),
                critical_free_bytes: Some(u64::MAX),
            },
        );
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
            enabled: true,
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
            enabled: true,
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
        let merged = merge_floors(
            &off,
            SessionFloors {
                warn_free_bytes: Some(32 * GIB_U64),
                critical_free_bytes: Some(16 * GIB_U64),
            },
        );
        assert!(!merged.enabled);
        // And the pure verdict still proceeds at every reading.
        assert_eq!(evaluate("host", Some(0), &merged), SpawnGate::Proceed);
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
        let fleet = SessionFloors {
            warn_free_bytes: Some(6 * GIB_U64),
            critical_free_bytes: None,
        };
        assert_eq!(
            evaluate("host", reading, &merge_floors(&local, fleet)),
            SpawnGate::Warn {
                lane: "host".to_string(),
                free_bytes: 4 * GIB_U64,
                floor_bytes: 6 * GIB_U64,
            }
        );
    }
}
