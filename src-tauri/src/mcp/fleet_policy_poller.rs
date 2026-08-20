//! Device-scoped poller for the fleet-policy domains this runner consumes:
//! `install_interception` (P3 + P4 of
//! `2026-06-08-fleet-policy-channel-redesign.md`), `fleet_resources` (Part B
//! item 3 of `2026-08-07-runner-resource-guard-and-session-protection.md`) and
//! `plan_capture` (Phase 4 of `2026-08-10-plan-and-prompt-library-in-web.md`).
//!
//! Coord exposes `GET /coord/fleet-policy?domain=<d>` (FleetPrincipal /
//! device-JWT gated) which returns the EFFECTIVE interception level resolved
//! for THIS device's tenant/fleet, alongside the tenant's §D1 fleet-resource
//! `controls` object:
//!
//! ```json
//! {"domain":"install_interception","effective_level":"off|observe|gate",
//!  "master_enabled":true,"resolved_scope":"...",
//!  "controls_available":true,
//!  "controls":{"min_free_bytes_sessions_host":3221225472, "...":null}}
//! ```
//!
//! This module owns THREE process-global caches and one supervised background
//! loop that refreshes all of them every [`POLL_INTERVAL`]:
//!
//! 1. The `effective_level`, read SYNCHRONOUSLY (no app state, no async) by
//!    `install_effects_producer::run_with_base` via
//!    [`effective_install_intercept_mode`] so the interception pre-call can make
//!    the per-install mode DYNAMIC (P4) rather than trusting the shim's
//!    spawn-time `QONTINUI_INSTALL_INTERCEPT_MODE` env.
//! 2. The tenant-wide **session-protection floors** ([`SessionFloors`], one set
//!    per lane), read just as synchronously by
//!    [`crate::resource_guard::effective_session_floors`] — which is on the
//!    spawn path, and therefore must never make a network call. The floors reach
//!    the gate as the *fleet default* term of
//!    `max(local override, fleet default, hardcoded default)`; the spawn path
//!    reads this cache and nothing else, so it produces a correct verdict with
//!    zero coord connectivity (the plan's local-first requirement, §Dependencies
//!    "Local-first requirement, restated"). A cache that is empty because coord
//!    was unreachable simply contributes no term.
//! 3. The tenant-wide **plan-capture level** (`off` | `record`), read just as
//!    synchronously by [`crate::terminal::runner_context`] — which renders the
//!    system-prompt briefing at spawn time and therefore must not make a network
//!    call either. At `record` the briefing gains the plan-library capture
//!    clause; at `off` (the resting value, and every fail-safe path) the clause
//!    is ABSENT, because an instruction with no live authorization must not
//!    appear in a system prompt.
//!
//! ## Lifecycle parallel to `device_jwt_refresher`
//!
//! Mirrors `mcp::device_jwt_refresher`'s shape so the call sites read
//! consistently:
//!
//! - [`PollerState`] holds the `watch` shutdown channel + join handle.
//! - [`start_poller`] spawns the loop via `task_supervisor::spawn_supervised`
//!   so a panic self-heals (a dead poller would silently freeze BOTH caches —
//!   see the refresher's supervisor rationale).
//! - [`commands::auto_start_fleet_policy_poller`] is the idempotent boot entry
//!   wired beside `auto_start_device_jwt_refresher` in `mcp_api`.
//!
//! ## Fail-safe contract (D7)
//!
//! For the interception mode:
//!
//! - Before the FIRST successful poll, the cache reads **`off`** (NEVER gate).
//! - A poll ERROR (network / decode / non-2xx) keeps the LAST-GOOD value.
//! - A coord **404 / 401 / auth-required** resets the cache to **`off`** (the
//!   policy is absent or this device isn't authorized — never gate).
//! - If there is no device JWT yet (unpaired) the poll is SKIPPED quietly —
//!   no log spam — like the other daemons that no-op without a token.
//! - Degradation is logged ONCE (a level transition), not every tick.
//!
//! For the session floors, the SAME posture, with "no fleet opinion" playing
//! the role `off` plays above — it is the value that cannot make the runner do
//! anything it would not have done without this poller at all:
//!
//! - Before the FIRST successful poll the cache holds **no floors**, so the
//!   effective floor is `max(local override, hardcoded default)` exactly as it
//!   was before this poller existed.
//! - A poll ERROR keeps the LAST-GOOD floors.
//! - A coord **404 / 401 / auth-required** resets the cache to **no floors**.
//! - Unpaired ⇒ SKIPPED quietly, cache untouched.
//! - Degradation is logged ONCE, on a transition.
//! - An ABSENT control field (coord predates the columns, or the tenant never
//!   set one) contributes **NOTHING** — it is UNKNOWN, and UNKNOWN is not a
//!   value. It is never folded in as a confident `0`, because a zero floor
//!   disables the guard it names. This is the arm that runs TODAY: the columns
//!   ship in qontinui-web `sess_guard_01` and coord reads them in its own later
//!   PR, so until both land `controls` simply carries the six older §D1 fields
//!   and the four floor fields decode to `None`.
//!
//! For the plan-capture level, the SAME posture with `off` again playing the
//! safe role — and here it is load-bearing in a way worth stating, because the
//! cache's value decides whether an INSTRUCTION appears in an agent's system
//! prompt:
//!
//! - Before the FIRST successful poll the cache reads **`off`**, so a runner
//!   that has never reached coord injects no clause.
//! - A poll ERROR keeps the LAST-GOOD level.
//! - A coord **404 / 401 / auth-required** resets to **`off`**. With no
//!   `plan_capture` row at all coord answers 404, which is precisely the
//!   "unconfigured tenant" case — and it must read as "do not instruct", never
//!   as "record".
//! - Unpaired ⇒ SKIPPED quietly, cache untouched.
//! - Any level that is not exactly `record` (including `observe`, `gate`, a
//!   typo, or an empty string) normalizes to `off`: this domain's vocabulary is
//!   two-valued and an unrecognised level is not an authorization.
//! - Degradation is logged ONCE, on a transition.
//! - A poisoned lock degrades to `off`.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use crate::mcp::types::ApiState;

/// How often the loop refreshes the cached effective level. 45s sits in the
/// 30–60s window the plan specifies — long enough to not hammer coord, short
/// enough that an operator flipping the fleet policy sees it take effect on
/// already-injected terminals within a minute.
const POLL_INTERVAL: Duration = Duration::from_secs(45);

/// The fleet-policy domain carrying the install-interception level.
const DOMAIN: &str = "install_interception";

/// The fleet-policy domain that OWNS the §D1 fleet-resource `controls` object,
/// including the four session-protection floors this poller caches
/// (`qontinui-coord/src/fleet_policy.rs`, `CONTROLS_DOMAIN`).
///
/// Polled as a SECOND request per tick rather than read off the
/// `install_interception` response. Coord's `read_controls` today answers with
/// the tenant's controls row whichever domain was asked for — but that is a
/// property of coord's current implementation, not of the wire contract, and it
/// is not something a cache on the spawn path should depend on. Asking the
/// domain that owns the columns keeps the request self-describing and keeps this
/// cache correct if coord ever scopes the controls read to its own domain. The
/// cost is one extra GET per [`POLL_INTERVAL`].
const CONTROLS_DOMAIN: &str = "fleet_resources";

/// The fleet-policy domain carrying the plan/prompt **capture** level
/// (`2026-08-10-plan-and-prompt-library-in-web.md` Phase 4, D4).
///
/// `coord.fleet_runtime_policy.domain` is plain `TEXT` with no enum, no CHECK
/// and no coord-side allowlist (qontinui-web's
/// `fleet_policy_01_coord_fleet_runtime_policy.py` creates the column; coord's
/// only `req.domain ==` comparison is an advisory soak branch scoped to
/// `install_interception`), so a third domain is **data, not schema** — nothing
/// ships in coord for this.
///
/// Scope band is **tenant-wide**: the clause is baked into the briefing once per
/// session at spawn time, and a session is not repo-scoped, so a `repo` band
/// would have no resolvable `scope_key` at the moment the decision is made.
const PLAN_CAPTURE_DOMAIN: &str = "plan_capture";

/// The fail-safe default: every read before the first success, and every
/// reset on a 404/401/auth-required, collapses to this. NEVER `gate`.
const DEFAULT_MODE: &str = "off";

/// The only level that TURNS PLAN CAPTURE ON. The domain's vocabulary is
/// two-valued (`off` | `record`); everything else normalizes to
/// [`DEFAULT_PLAN_CAPTURE_LEVEL`].
///
/// [`crate::terminal::runner_context`] compares the cached level against it to
/// decide whether to append the capture clause. The word is the contract
/// between the normalizer that WRITES the cache and the briefing that READS it,
/// so it has exactly one definition rather than a matching pair that a rename
/// could silently split.
///
/// `pub`, not `pub(crate)`: this module moved into the lib crate, and `main.rs`
/// — now a separate crate — still reads it. It is exported TOGETHER with
/// [`effective_plan_capture_level`] deliberately; the pairing below is the
/// point, and neither should be exported without the other.
pub const PLAN_CAPTURE_RECORD: &str = "record";

/// The fail-safe default for [`PLAN_CAPTURE_DOMAIN`]. Spelled separately from
/// [`DEFAULT_MODE`] even though both are `"off"`: they are different domains'
/// vocabularies that merely coincide today, and collapsing them would let a
/// future edit to one silently move the other.
const DEFAULT_PLAN_CAPTURE_LEVEL: &str = "off";

// ===========================================================================
// Process-global cache
// ===========================================================================

/// The cached effective interception mode (`off` | `observe` | `gate`).
/// `RwLock<String>` behind a `OnceLock` — `once_cell` is already a dep, but
/// `std::sync::OnceLock` is in std since 1.70 and is the same idiom the
/// `device_jwt_refresher::commands` holder uses, so we stay consistent.
static EFFECTIVE_MODE: OnceLock<RwLock<String>> = OnceLock::new();

fn cache() -> &'static RwLock<String> {
    EFFECTIVE_MODE.get_or_init(|| RwLock::new(DEFAULT_MODE.to_string()))
}

/// Read the current effective install-interception mode. Returns `"off"`
/// until the first successful poll (and after any auth/absent reset).
///
/// SYNCHRONOUS + lock-only — safe to call from `run_with_base` (which has no
/// app state and is not the place to do async I/O). A poisoned lock degrades
/// fail-safe to `"off"`.
pub fn effective_install_intercept_mode() -> String {
    cache()
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| DEFAULT_MODE.to_string())
}

/// Overwrite the cached mode. Internal — only the poll loop calls this.
fn set_mode(mode: &str) {
    if let Ok(mut g) = cache().write() {
        *g = mode.to_string();
    }
}

/// Test-only cache setter so other modules' tests (e.g.
/// `install_effects_producer`'s intercept-mode tests, which need a non-`off`
/// effective mode for the pre-call to proceed past the P4 short-circuit) can
/// pin the process-global cache. NEVER compiled into a release binary.
#[cfg(test)]
pub(crate) fn set_mode_for_test(mode: &str) {
    set_mode(mode);
}

// ===========================================================================
// Process-global cache #2 — the tenant-wide session-protection floors
// ===========================================================================

/// The fleet's session-protection floors for ONE lane, in bytes.
///
/// Both fields are `Option` and the `None` arm is the whole point: it means the
/// fleet has **no opinion**, which is what an absent column, an unprovisioned
/// coord, an unreachable coord and an unpaired runner all produce. `None` folds
/// into [`crate::resource_guard::merge_floors`] as *nothing*, never as `0` — a
/// zero floor disables the guard it names, so rendering UNKNOWN as zero would
/// turn a missing fleet term into a silently disabled protection.
///
/// The quantity is lane-specific and the lanes are NEVER summed or substituted
/// for one another (`fleet::resource_sample`'s module docs): the host figure is
/// Windows free commit, the WSL figure is that VM's available memory. A gate
/// judging a host reading must compare it against the host floor or against
/// nothing at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionFloors {
    /// Warn floor — below this, starting another interactive session is
    /// discouraged but allowed.
    pub(crate) warn_free_bytes: Option<u64>,
    /// Critical floor — below this, a new spawn is refused by default (always
    /// overridable at the point of refusal).
    pub(crate) critical_free_bytes: Option<u64>,
}

/// Both lanes' floors, as one cached value so a poll swaps them atomically and
/// a reader can never see the host lane from one poll beside the WSL lane from
/// another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SessionFloorsByLane {
    host: SessionFloors,
    wsl: SessionFloors,
}

impl SessionFloorsByLane {
    /// The floors for `lane`, or NO floors for a lane name this runner does not
    /// know. Fail-safe: an unrecognised lane is UNKNOWN, and the one thing that
    /// must never happen is judging one lane's reading against another lane's
    /// floor. The names come from [`crate::fleet::resource_sample::Lane`] rather
    /// than from literals here so the vocabulary has exactly one home.
    fn for_lane(&self, lane: &str) -> SessionFloors {
        use crate::fleet::resource_sample::Lane;
        if lane == Lane::Host.as_str() {
            self.host
        } else if lane == Lane::Wsl.as_str() {
            self.wsl
        } else {
            SessionFloors::default()
        }
    }
}

/// The cached fleet floors. Resting value is `Default` — every field `None`,
/// i.e. "the fleet has said nothing", which is the fail-safe value in exactly
/// the sense [`DEFAULT_MODE`] is: it cannot make the runner do anything it
/// would not do with this poller switched off entirely.
static SESSION_FLOORS: OnceLock<RwLock<SessionFloorsByLane>> = OnceLock::new();

fn floors_cache() -> &'static RwLock<SessionFloorsByLane> {
    SESSION_FLOORS.get_or_init(|| RwLock::new(SessionFloorsByLane::default()))
}

/// Read the fleet's cached session floors for `lane`.
///
/// SYNCHRONOUS + lock-only, like [`effective_install_intercept_mode`] — this is
/// called from [`crate::resource_guard::probe_for_spawn`], which runs
/// immediately before a PTY is opened and must not do I/O of any kind. A
/// poisoned lock degrades to "no floors", the same fail-safe the initial value
/// carries.
pub(crate) fn fleet_session_floors(lane: &str) -> SessionFloors {
    floors_cache()
        .read()
        .map(|g| g.for_lane(lane))
        .unwrap_or_default()
}

/// Overwrite the cached floors. Internal — only the poll loop calls this.
fn set_floors(floors: SessionFloorsByLane) {
    if let Ok(mut g) = floors_cache().write() {
        *g = floors;
    }
}

// ===========================================================================
// Process-global cache #3 — the tenant-wide plan-capture level
// ===========================================================================

/// The cached plan-capture level (`off` | `record`).
static PLAN_CAPTURE_LEVEL: OnceLock<RwLock<String>> = OnceLock::new();

/// The cache's INITIAL value, as a named function rather than an inline
/// closure. This is the single most consequential expression in the module —
/// it decides what a runner that has never reached coord puts in a system
/// prompt — so it is callable from a test, which an inline closure is not.
fn new_plan_capture_cache() -> RwLock<String> {
    RwLock::new(DEFAULT_PLAN_CAPTURE_LEVEL.to_string())
}

fn plan_capture_cache() -> &'static RwLock<String> {
    PLAN_CAPTURE_LEVEL.get_or_init(new_plan_capture_cache)
}

/// Read a cached level, degrading a POISONED lock to `fallback`. PURE apart
/// from the lock read, and named rather than inlined so a test can drive the
/// poison arm through the SHIPPING expression, against a throwaway lock, rather
/// than asserting about a copy of it.
fn read_cached_level(cache: &RwLock<String>, fallback: &str) -> String {
    cache
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| fallback.to_string())
}

/// Read the current effective plan-capture level. Returns `"off"` until the
/// first successful poll (and after any auth/absent reset).
///
/// SYNCHRONOUS + lock-only — safe to call from
/// [`crate::terminal::runner_context`], which renders the spawn-time briefing
/// and must not do I/O. A poisoned lock degrades fail-safe to `"off"`, i.e. the
/// capture clause is omitted.
///
/// The level is only meaningful next to [`PLAN_CAPTURE_RECORD`], so the two are
/// exported as a pair: handing a caller the reader without the vocabulary would
/// give it a string it cannot correctly compare. Both were `pub(crate)` until
/// this module moved into the lib crate; `main.rs` is a separate crate now and
/// still calls this, so both widened together rather than one of them.
pub fn effective_plan_capture_level() -> String {
    read_cached_level(plan_capture_cache(), DEFAULT_PLAN_CAPTURE_LEVEL)
}

/// Overwrite the cached plan-capture level. Internal — only the poll loop calls
/// this (and the test guard below).
fn set_plan_capture_level(level: &str) {
    if let Ok(mut g) = plan_capture_cache().write() {
        *g = level.to_string();
    }
}

/// Test-only RAII pin over the process-global plan-capture level.
///
/// Two jobs, both of which a bare setter got wrong:
///
/// 1. It SERIALIZES. The level is process-global, so two tests pinning it
///    concurrently under cargo's thread pool would decide each other's outcome.
///    The mutex lives HERE, beside the cache, so every module that pins the
///    level shares one — a mutex private to `terminal::tests` cannot be taken
///    by `agent_runtime`'s briefing test, which renders the same string.
/// 2. It RESTORES on `Drop`, so a FAILING assertion (which panics past any
///    trailing restore statement) cannot leak `record` into the next test and
///    turn one real failure into a cascade. Poisoning is deliberately absorbed
///    for the same reason: the guard's whole point is that the next test starts
///    from the fail-safe value whatever happened to the last one.
///
/// NEVER compiled into a release binary.
#[cfg(test)]
pub(crate) struct PlanCaptureLevelPin(std::sync::MutexGuard<'static, ()>);

#[cfg(test)]
impl PlanCaptureLevelPin {
    /// Pin the level to `level`. Callable repeatedly under one guard for a test
    /// that sweeps several levels.
    pub(crate) fn set(&self, level: &str) {
        set_plan_capture_level(level);
    }
}

#[cfg(test)]
impl Drop for PlanCaptureLevelPin {
    fn drop(&mut self) {
        set_plan_capture_level(DEFAULT_PLAN_CAPTURE_LEVEL);
    }
}

/// Acquire the pin. Blocks until any other pinning test has released it, and
/// restores the fail-safe level when the returned guard drops.
#[cfg(test)]
pub(crate) fn pin_plan_capture_level_for_test(level: &str) -> PlanCaptureLevelPin {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    let guard = LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        // A poisoned mutex means a PREVIOUS test panicked while holding it. Its
        // `Drop` already restored the level, so the data is fine and blocking
        // every later test on it would only hide the original failure.
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let pin = PlanCaptureLevelPin(guard);
    pin.set(level);
    pin
}

/// Normalize coord's `effective_level` onto this domain's TWO levels. PURE.
///
/// Exactly `record` (case-insensitively, trimmed) turns capture on. Absent,
/// null, empty, and every unrecognised value — including the interception
/// domain's `observe` / `gate`, which mean nothing here — collapse to `off`.
/// Never honor a level we cannot identify as an authorization to instruct.
fn normalize_plan_capture_level(raw: Option<&str>) -> String {
    let level = raw
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if level == PLAN_CAPTURE_RECORD {
        PLAN_CAPTURE_RECORD.to_string()
    } else {
        DEFAULT_PLAN_CAPTURE_LEVEL.to_string()
    }
}

/// The WRITE, if any, that `outcome` implies for the plan-capture cache. PURE —
/// this is the fail-safe contract itself, factored out of the loop so it is
/// directly testable rather than asserted about a re-implementation of the
/// loop's `match`.
///
/// `None` means **do not write at all**, which is stronger than writing the
/// last-good value back: "last-good is kept" then holds because no write
/// happened, not because a read-then-write round trip happened to compute the
/// same string. That also keeps this domain byte-identical in behaviour to the
/// two siblings, whose `Skipped`/`Kept` arms take no write lock either.
fn next_plan_capture_level(outcome: &PollOutcome) -> Option<String> {
    match outcome {
        PollOutcome::Updated(level) => Some(level.clone()),
        // Absent policy / unauthorized device ⇒ never instruct.
        PollOutcome::ResetOff(_) => Some(DEFAULT_PLAN_CAPTURE_LEVEL.to_string()),
        // Unpaired or a transient failure ⇒ the cache is left exactly as it was.
        PollOutcome::SkippedNoJwt | PollOutcome::Kept(_) => None,
    }
}

/// The edge-trigger KEY for a plan-capture outcome — what has to change before
/// another log line is emitted. PURE.
///
/// Not the outcome itself: [`PollOutcome::Kept`] carries the formatted reqwest
/// error, and a message that varies tick to tick (a DNS detail, an `os error N`
/// variant, an ephemeral port) is a *different value* every 45s, which would
/// turn "logged ONCE on a transition" into a line per tick under exactly the
/// sustained-failure conditions the once-per-transition rule exists for. So a
/// repeated failure collapses to one key, while a changed LEVEL or a changed
/// STATUS — the two things an operator needs to see — still open a new one.
fn plan_capture_log_key(outcome: &PollOutcome) -> String {
    match outcome {
        PollOutcome::Updated(level) => format!("updated:{level}"),
        PollOutcome::ResetOff(status) => format!("reset:{status}"),
        PollOutcome::SkippedNoJwt => "skipped".to_string(),
        PollOutcome::Kept(_) => "kept".to_string(),
    }
}

// ===========================================================================
// Wire type (coord response subset)
// ===========================================================================

/// Subset of coord's `GET /coord/fleet-policy` response we read. `master_enabled`
/// + `resolved_scope` are pulled through for observability but only
/// `effective_level` drives the cache. Every field defaults so a coord that
/// trims/renames a sibling field doesn't break the decode.
#[derive(Debug, Clone, serde::Deserialize)]
struct FleetPolicyResponse {
    #[serde(default)]
    effective_level: Option<String>,
    /// Coord's own "I could not read the control columns" flag. `Some(false)`
    /// means the §D1 columns are not provisioned on that deployment yet, which
    /// is NOT the same statement as "the tenant set no floors" — but both
    /// produce the same, correct behaviour here (no fleet term), so the flag is
    /// read for the explicitness rather than for a different outcome.
    #[serde(default)]
    controls_available: Option<bool>,
    /// The §D1 controls object, `null` when coord cannot read the columns.
    #[serde(default)]
    controls: Option<ControlsPayload>,
}

/// The subset of coord's `controls` object this poller reads: the four
/// session-protection byte floors from the plan's Part B item 1
/// (qontinui-web alembic revision `sess_guard_01`, coord's `CONTROL_COLS`
/// widening in its companion PR).
///
/// Deliberately NOT `#[serde(deny_unknown_fields)]` — coord's own
/// `FleetPolicyControls` denies unknown fields because it is a REQUEST type
/// where a typo'd control name must be rejected at the door; this is a RESPONSE
/// subset, where the six older §D1 controls (`min_free_mem_bytes_host`,
/// `sample_interval_secs`, …) and anything coord adds later must decode past us
/// untouched.
///
/// Every field is `Option<i64>` with a `#[serde(default)]`, which is what makes
/// the pre-migration wire — a `controls` object that carries the six older
/// fields and none of these four — decode cleanly to "no fleet opinion" rather
/// than to a decode error. `i64` rather than `u64` because the columns are
/// `BIGINT` and coord types them `Option<i64>`; the sign is dealt with in
/// [`floor_bytes`], not by a lossy cast.
#[derive(Debug, Clone, serde::Deserialize)]
struct ControlsPayload {
    #[serde(default)]
    min_free_bytes_sessions_host: Option<i64>,
    #[serde(default)]
    min_free_bytes_sessions_wsl: Option<i64>,
    #[serde(default)]
    min_free_bytes_sessions_critical_host: Option<i64>,
    #[serde(default)]
    min_free_bytes_sessions_critical_wsl: Option<i64>,
}

/// One control column → a floor this runner will honour. PURE.
///
/// - Absent / SQL NULL ⇒ `None`. "No override, never zero" is what the column
///   comment on the migration says, and it is the whole reason this returns an
///   `Option` instead of a `u64` with a zero default.
/// - NEGATIVE ⇒ `None`. Coord's `validate()` rejects a negative byte floor at
///   the door, so this can only arrive from a hand-edited row — and the failure
///   mode of `as u64` on a negative is a floor of ~18 exabytes, which would
///   block every spawn on every machine in the tenant. An impossible value is
///   UNKNOWN, and UNKNOWN contributes nothing.
/// - Zero ⇒ `Some(0)`, preserved rather than folded into `None`: the fleet is
///   entitled to say zero, and saying it changes nothing, because the effective
///   floor is a `max` that already contains the hardcoded default.
///
/// The SIGN is all this checks. An absurd *positive* — the column is a `BIGINT`
/// and coord's `validate()` rejects only negatives, so `i64::MAX` is a legal row
/// — is not filtered here, because this function's job is decoding one column
/// and "how high may a floor be?" is a policy question about the *effective*
/// floor, which is composed of three terms. It is answered once, downstream, by
/// [`crate::resource_guard::SESSION_FLOOR_MAX_BYTES`], which caps the fold and
/// therefore bounds a local override and a fleet column with the same number.
fn floor_bytes(v: Option<i64>) -> Option<u64> {
    v.and_then(|b| u64::try_from(b).ok())
}

/// Fold a decoded response into the lane-separated floors to cache. PURE.
///
/// Every path that is not "coord handed us a controls object with these fields
/// populated" yields `SessionFloorsByLane::default()` — no floors — because
/// that is the value with no consequence. That includes the path that runs
/// today, before coord ships its half: `controls` present, the four fields
/// absent.
fn session_floors_from(body: &FleetPolicyResponse) -> SessionFloorsByLane {
    // Coord explicitly said it cannot read the columns. `controls` is `null` in
    // that case anyway; reading the flag makes the intent legible instead of
    // inferring it from a null.
    if body.controls_available == Some(false) {
        return SessionFloorsByLane::default();
    }
    let Some(controls) = body.controls.as_ref() else {
        return SessionFloorsByLane::default();
    };
    SessionFloorsByLane {
        host: SessionFloors {
            warn_free_bytes: floor_bytes(controls.min_free_bytes_sessions_host),
            critical_free_bytes: floor_bytes(controls.min_free_bytes_sessions_critical_host),
        },
        wsl: SessionFloors {
            warn_free_bytes: floor_bytes(controls.min_free_bytes_sessions_wsl),
            critical_free_bytes: floor_bytes(controls.min_free_bytes_sessions_critical_wsl),
        },
    }
}

/// Compact, log-friendly rendering of the cached floors.
///
/// An absent floor prints `unset`, never `0`. The log line is the only place an
/// operator can see whether a fleet floor is missing or genuinely zero, and
/// printing UNKNOWN as `0` in the one surface that exists to answer that
/// question would be the same lie the type system is arranged to prevent.
fn describe_floors(floors: &SessionFloorsByLane) -> String {
    fn one(v: Option<u64>) -> String {
        v.map_or_else(|| "unset".to_string(), |b| b.to_string())
    }
    format!(
        "host warn={} critical={}, wsl warn={} critical={}",
        one(floors.host.warn_free_bytes),
        one(floors.host.critical_free_bytes),
        one(floors.wsl.warn_free_bytes),
        one(floors.wsl.critical_free_bytes),
    )
}

/// Subset of coord's `GET /health` response we read for the capability probe.
///
/// Coord ships a `capabilities` object (parallel coord PR), e.g.
/// `{"capabilities":{"install_signatures":true,"fleet_policy":true}}`. We read
/// ONLY `fleet_policy` to decide whether this poller should run at all.
///
/// FAIL-SAFE DECODE (the absent-field=capable default): `capabilities` is
/// `Option` so a coord that PREDATES the field (no `capabilities` key) decodes
/// to `None` ⇒ we ASSUME capable and poll as normal. Within `capabilities`,
/// `fleet_policy` is `Option<bool>` so only an EXPLICIT `false` disables the
/// poller; an absent `fleet_policy` key (coord ships other caps but not this
/// one yet) also assumes capable. We never break against a coord that doesn't
/// know the field.
#[derive(Debug, Clone, serde::Deserialize)]
struct HealthResponse {
    #[serde(default)]
    capabilities: Option<Capabilities>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Capabilities {
    #[serde(default)]
    fleet_policy: Option<bool>,
}

// ===========================================================================
// Poller state + supervised loop
// ===========================================================================

/// State for the fleet-policy poller task. Owns the shutdown channel + join
/// handle so the boot entry can stop / restart it. (No kick channel: unlike
/// the refresher there's no event that needs to wake it early — the policy is
/// pull-only on a fixed cadence.)
pub struct PollerState {
    shutdown_tx: watch::Sender<bool>,
    task_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PollerState {
    /// Stop the poller task, giving it up to 3 seconds to shut down cleanly.
    pub async fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(handle) = self.task_handle.lock().await.take() {
            match tokio::time::timeout(Duration::from_secs(3), handle).await {
                Ok(_) => info!("Fleet-policy poller stopped gracefully"),
                Err(_) => {
                    warn!("Fleet-policy poller did not stop in 3s; shutdown signal sent, moving on")
                }
            }
        }
    }
}

/// Spawn the poller task. Returns the state handle so the caller can stop it.
pub fn start_poller(api_state: Arc<ApiState>) -> Arc<PollerState> {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // SUPERVISOR. Same rationale as `device_jwt_refresher::start_refresher`:
    // `poller_loop` is long-lived and should only RETURN on shutdown. A bare
    // panic would PERMANENTLY freeze BOTH caches at their last values (which,
    // worse, could be a stale `gate` after the operator turned the policy off
    // — leaving every terminal blocking installs — or a stale session floor the
    // fleet has since lowered). Supervise it so a panic/wedge self-heals
    // instead of requiring a runner restart.
    let shutdown_rx_loop = shutdown_rx.clone();
    let task_handle = crate::mcp::task_supervisor::spawn_supervised(
        "Fleet-policy poller",
        shutdown_rx,
        move || poller_loop(api_state.clone(), shutdown_rx_loop.clone()),
    );

    Arc::new(PollerState {
        shutdown_tx,
        task_handle: Mutex::new(Some(task_handle)),
    })
}

/// Outcome of a single poll attempt. Factored out so the loop's logging stays
/// edge-triggered (log only on a transition, never every tick).
///
/// SHARED by both LEVEL-carrying domains ([`DOMAIN`] and
/// [`PLAN_CAPTURE_DOMAIN`]) — they cache different vocabularies but classify
/// coord's answers identically, and one enum is what keeps them from drifting
/// apart on what a 404 means. ([`CONTROLS_DOMAIN`] needs its own because it
/// carries floors, not a level.)
#[derive(Debug, Clone, PartialEq, Eq)]
enum PollOutcome {
    /// Coord returned 2xx with a level — cache updated to this value.
    Updated(String),
    /// No device JWT yet (unpaired) — poll skipped, cache untouched.
    SkippedNoJwt,
    /// Coord said 401 / 404 / auth-required — cache RESET to `off` (fail-safe).
    ResetOff(u16),
    /// Network / decode / other non-2xx error — LAST-GOOD value kept.
    Kept(String),
}

/// Outcome of a single poll of the [`CONTROLS_DOMAIN`]. Same four arms as
/// [`PollOutcome`], carrying the floors instead of a level — the two caches
/// share one fail-safe contract, and sharing the SHAPE of the outcome is what
/// keeps them from drifting apart the next time one of them is edited.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlsOutcome {
    /// Coord returned 2xx — cache replaced with these floors (possibly "none",
    /// which is a legitimate answer, not an error).
    Updated(SessionFloorsByLane),
    /// No device JWT yet (unpaired) — poll skipped, cache untouched.
    SkippedNoJwt,
    /// Coord said 401 / 404 — cache RESET to no floors (fail-safe).
    Reset(u16),
    /// Network / decode / other non-2xx error — LAST-GOOD floors kept.
    Kept(String),
}

/// Why a fetch produced no body.
///
/// Factored out so both domains map coord's answers onto their caches through
/// ONE decision: a 401 resets, an error keeps, an absent JWT skips. The two
/// caches are allowed to hold different values; they are not allowed to disagree
/// about what a 404 means.
#[derive(Debug)]
enum FetchError {
    /// No device JWT yet — this runner is unpaired.
    NoJwt,
    /// Coord said 401 / 404: the policy is absent, or this device isn't
    /// authorized.
    AuthOrAbsent(u16),
    /// Network / decode / other non-2xx, with the operator-readable detail.
    Failed(String),
}

/// Outcome of the one-shot capability probe done at poller start.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CapabilityCheck {
    /// Coord advertises `capabilities` and `fleet_policy` is EXPLICITLY false —
    /// the ONLY case that disables the poller.
    Disabled,
    /// Capable: coord said `fleet_policy:true`, OR `fleet_policy` was absent
    /// from a present `capabilities`, OR there was no `capabilities` field at
    /// all (older coord), OR the probe errored. Fail-safe — when in doubt, poll.
    Capable,
}

/// One-shot `GET /health` capability probe (§5). Decides whether this poller
/// should run at all by reading coord's `capabilities.fleet_policy`.
///
/// DEFENSIVE / FAIL-SAFE CONTRACT:
/// - `fleet_policy == Some(false)` (explicitly disabled) ⇒ [`CapabilityCheck::Disabled`].
/// - `fleet_policy == Some(true)` ⇒ Capable.
/// - `fleet_policy` ABSENT but `capabilities` present ⇒ Capable (coord ships
///   other caps but not this flag yet — don't disable).
/// - NO `capabilities` field at all (coord predates it) ⇒ Capable.
/// - ANY error (request / non-2xx / decode) ⇒ Capable. We never disable the
///   poller because we couldn't reach `/health`. (The coord base itself can no
///   longer fail to resolve: `profiles::coord_base_with_source` always yields
///   one.)
async fn check_fleet_policy_capability() -> CapabilityCheck {
    let base = crate::profiles::coord_base_with_source().0;
    let url = format!("{}/health", base.trim_end_matches('/'));

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return CapabilityCheck::Capable,
    };

    // `/health` is an unauthenticated liveness endpoint — no Bearer needed.
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => return CapabilityCheck::Capable,
    };
    if !resp.status().is_success() {
        return CapabilityCheck::Capable;
    }
    let body: HealthResponse = match resp.json().await {
        Ok(b) => b,
        Err(_) => return CapabilityCheck::Capable,
    };

    match body.capabilities {
        // capabilities present + fleet_policy explicitly false ⇒ the only
        // disable case.
        Some(caps) if caps.fleet_policy == Some(false) => CapabilityCheck::Disabled,
        // capabilities present, fleet_policy true or absent ⇒ capable.
        // No capabilities field at all (older coord) ⇒ capable.
        _ => CapabilityCheck::Capable,
    }
}

/// Is `current` a TRANSITION away from the last outcome we logged? PURE.
///
/// The edge-trigger behind "degradation is logged ONCE, not every tick". Shared
/// by all three domains and factored out of the loop so the property is
/// testable — a steady state must emit exactly one line however many ticks it
/// spans, and `None` (nothing logged yet) is always a transition.
fn is_new_outcome<T: PartialEq>(last: Option<&T>, current: &T) -> bool {
    last != Some(current)
}

async fn poller_loop(_api_state: Arc<ApiState>, mut shutdown_rx: watch::Receiver<bool>) {
    // §5 capability gate (one-shot at start): if coord EXPLICITLY advertises it
    // lacks the fleet_policy capability, stay off and don't poll. Any other
    // outcome (capable, absent flag, no capabilities field, or any probe error)
    // proceeds to poll as normal — defensive default never breaks against an
    // older coord that predates the `capabilities` field.
    //
    // We do NOT `return` on Disabled: the task supervisor respawns any loop that
    // returns without a shutdown signal (task_supervisor.rs:108), which would
    // re-run this probe + re-log every backoff window. Instead we log ONCE and
    // PARK on the shutdown channel — the cache already reads the fail-safe
    // `off`, so a non-polling parked loop is exactly the desired "stay off".
    if check_fleet_policy_capability().await == CapabilityCheck::Disabled {
        info!("fleet_policy_poller: coord lacks fleet_policy capability — staying off");
        // Park until shutdown (the cache stays at the fail-safe DEFAULT_MODE).
        let _ = shutdown_rx.changed().await;
        info!("Fleet-policy poller shutting down (was parked: coord lacks capability)");
        return;
    }

    info!(
        "Fleet-policy poller started (domains={DOMAIN},{CONTROLS_DOMAIN},{PLAN_CAPTURE_DOMAIN}, \
         interval={}s, fail-safe defaults: mode={DEFAULT_MODE}, session floors unset, \
         plan capture={DEFAULT_PLAN_CAPTURE_LEVEL})",
        POLL_INTERVAL.as_secs()
    );

    // Edge-trigger degradation logs: remember the LAST outcome class we logged
    // so a steady-state (e.g. repeated SkippedNoJwt while unpaired, or repeated
    // network errors) emits exactly one line, not one per tick. One per cache:
    // the two domains fail independently (coord can serve the interception level
    // from a deployment whose control columns are not provisioned), so a shared
    // marker would suppress one cache's transition because the other's had
    // already been logged.
    let mut last_logged: Option<PollOutcome> = None;
    let mut last_logged_controls: Option<ControlsOutcome> = None;
    // A KEY, not an outcome — see `plan_capture_log_key` for why the whole
    // value is the wrong thing to compare for this domain.
    let mut last_logged_plan_capture: Option<String> = None;

    loop {
        if *shutdown_rx.borrow() {
            info!("Fleet-policy poller shutting down");
            return;
        }

        let outcome = poll_once().await;

        // Apply the cache effect.
        match &outcome {
            PollOutcome::Updated(level) => set_mode(level),
            PollOutcome::ResetOff(_) => set_mode(DEFAULT_MODE),
            // Skipped / Kept leave the cache as-is (last-good or default).
            PollOutcome::SkippedNoJwt | PollOutcome::Kept(_) => {}
        }

        // Log only on a class change so we don't spam every 45s.
        let changed = is_new_outcome(last_logged.as_ref(), &outcome);
        if changed {
            match &outcome {
                PollOutcome::Updated(level) => {
                    info!("fleet_policy_poller: effective install-interception level = {level}");
                }
                PollOutcome::SkippedNoJwt => {
                    info!(
                        "fleet_policy_poller: no device JWT yet (unpaired) — skipping poll, \
                         interception mode stays {DEFAULT_MODE}"
                    );
                }
                PollOutcome::ResetOff(status) => {
                    warn!(
                        "fleet_policy_poller: coord returned {status} (auth/absent) — \
                         resetting interception mode to {DEFAULT_MODE} (fail-safe, never gate)"
                    );
                }
                PollOutcome::Kept(err) => {
                    warn!(
                        "fleet_policy_poller: poll failed ({err}) — keeping last-good \
                         interception mode ({})",
                        effective_install_intercept_mode()
                    );
                }
            }
            last_logged = Some(outcome);
        }

        // Second domain: the tenant-wide session-protection floors. Polled after
        // the interception level and applied to its own cache, so neither
        // domain's failure can withhold the other's answer.
        let controls_outcome = poll_controls_once().await;

        match &controls_outcome {
            ControlsOutcome::Updated(floors) => set_floors(*floors),
            ControlsOutcome::Reset(_) => set_floors(SessionFloorsByLane::default()),
            // Skipped / Kept leave the cache as-is (last-good or "no floors").
            ControlsOutcome::SkippedNoJwt | ControlsOutcome::Kept(_) => {}
        }

        let controls_changed = is_new_outcome(last_logged_controls.as_ref(), &controls_outcome);
        if controls_changed {
            match &controls_outcome {
                ControlsOutcome::Updated(floors) => {
                    info!(
                        "fleet_policy_poller: fleet session floors ({}) = {}",
                        CONTROLS_DOMAIN,
                        describe_floors(floors)
                    );
                }
                ControlsOutcome::SkippedNoJwt => {
                    info!(
                        "fleet_policy_poller: no device JWT yet (unpaired) — skipping the \
                         {CONTROLS_DOMAIN} poll, session floors stay unset (the runner's own \
                         local + hardcoded floors still apply)"
                    );
                }
                ControlsOutcome::Reset(status) => {
                    warn!(
                        "fleet_policy_poller: coord returned {status} for {CONTROLS_DOMAIN} \
                         (auth/absent) — clearing the cached fleet session floors (fail-safe: \
                         no fleet term, never a zero floor)"
                    );
                }
                ControlsOutcome::Kept(err) => {
                    warn!(
                        "fleet_policy_poller: {CONTROLS_DOMAIN} poll failed ({err}) — keeping \
                         last-good fleet session floors"
                    );
                }
            }
            last_logged_controls = Some(controls_outcome);
        }

        // Third domain: the tenant-wide plan-capture level. Polled and applied
        // to its own cache for the same reason as the second — no domain's
        // failure may withhold another's answer.
        let plan_capture_outcome = poll_plan_capture_once().await;

        // The cache effect IS the fail-safe contract, so it lives in a pure,
        // tested function rather than in this `match`. `None` ⇒ no write at
        // all: Skipped / Kept leave the cache exactly as it was, taking no
        // write lock, exactly like the two sibling domains.
        if let Some(next_level) = next_plan_capture_level(&plan_capture_outcome) {
            set_plan_capture_level(&next_level);
        }

        // Keyed, not compared whole — see `plan_capture_log_key`.
        let plan_capture_key = plan_capture_log_key(&plan_capture_outcome);
        if is_new_outcome(last_logged_plan_capture.as_ref(), &plan_capture_key) {
            match &plan_capture_outcome {
                PollOutcome::Updated(level) => {
                    info!(
                        "fleet_policy_poller: effective plan-capture level ({PLAN_CAPTURE_DOMAIN}) \
                         = {level} (briefing clause {})",
                        if level == PLAN_CAPTURE_RECORD {
                            "injected"
                        } else {
                            "omitted"
                        }
                    );
                }
                PollOutcome::SkippedNoJwt => {
                    info!(
                        "fleet_policy_poller: no device JWT yet (unpaired) — skipping the \
                         {PLAN_CAPTURE_DOMAIN} poll, plan-capture level stays \
                         {DEFAULT_PLAN_CAPTURE_LEVEL}"
                    );
                }
                PollOutcome::ResetOff(status) => {
                    // 404 is the NORMAL answer for a tenant that never set this
                    // policy, so this is info, not warn — the clause is simply
                    // not authorized.
                    info!(
                        "fleet_policy_poller: coord returned {status} for {PLAN_CAPTURE_DOMAIN} \
                         (auth/absent) — plan-capture level reset to \
                         {DEFAULT_PLAN_CAPTURE_LEVEL} (fail-safe: never instruct)"
                    );
                }
                PollOutcome::Kept(err) => {
                    warn!(
                        "fleet_policy_poller: {PLAN_CAPTURE_DOMAIN} poll failed ({err}) — keeping \
                         last-good plan-capture level ({})",
                        effective_plan_capture_level()
                    );
                }
            }
            last_logged_plan_capture = Some(plan_capture_key);
        }

        // Sleep until the next tick, waking early on shutdown.
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("Fleet-policy poller shutting down");
                return;
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// One GET against coord for `domain`. Resolves the coord base the SAME way the
/// install-effects producer does (`profiles::coord_base_with_source()`) and
/// presents the device JWT from `AuthManager::get_access_token()` as the
/// `Authorization: Bearer` — the exact accessor `backend_relay` uses
/// (`backend_relay.rs:452`) to authenticate the device WS.
///
/// NET-NEW coord client (D4): the existing `device_jwt_refresher` talks to the
/// WEB-BACKEND proxy, not coord, so we issue our own GET here.
///
/// Parameterised by domain rather than duplicated per cache: the auth, the base
/// resolution and the 401/404-versus-error classification are the parts a second
/// copy would eventually get subtly wrong.
async fn fetch_fleet_policy(domain: &str) -> Result<FleetPolicyResponse, FetchError> {
    // Device JWT — same slot the relay reads as its Bearer (REPLACE-not-REVOKE
    // lifecycle owned by the refresher). Empty ⇒ unpaired ⇒ skip quietly.
    let device_jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return Err(FetchError::NoJwt),
    };

    // coord base — identical source-of-truth chain to the producer's
    // `coord_base()` (env COORD_HTTP_URL → profile coord_url → default).
    let base = crate::profiles::coord_base_with_source().0;
    let url = format!(
        "{}/coord/fleet-policy?domain={domain}",
        base.trim_end_matches('/')
    );

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(FetchError::Failed(format!("client build: {e}"))),
    };

    let resp = match client.get(&url).bearer_auth(&device_jwt).send().await {
        Ok(r) => r,
        Err(e) => return Err(FetchError::Failed(format!("request: {e}"))),
    };

    let status = resp.status();
    // Auth / absent ⇒ the caller's fail-safe reset (NEVER gate, NEVER a floor).
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::NOT_FOUND {
        return Err(FetchError::AuthOrAbsent(status.as_u16()));
    }
    if !status.is_success() {
        return Err(FetchError::Failed(format!(
            "coord status {}",
            status.as_u16()
        )));
    }

    resp.json()
        .await
        .map_err(|e| FetchError::Failed(format!("decode: {e}")))
}

/// Normalize coord's `effective_level` onto the three levels this runner acts
/// on. PURE.
///
/// Absent / null / empty ⇒ coord's documented "off when absent" contract — off,
/// as a cache update rather than an error. Anything unrecognised ⇒ off too:
/// never honor a level we cannot identify as a gate trigger.
fn normalize_level(raw: Option<&str>) -> String {
    let level = raw
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODE.to_string());
    match level.as_str() {
        "off" | "observe" | "gate" => level,
        _ => DEFAULT_MODE.to_string(),
    }
}

/// Map a fetch failure onto the level-cache outcome it implies. PURE.
///
/// Shared by both level-carrying domains so "a 401/404 resets, a network error
/// keeps, an absent JWT skips" is decided in exactly one place.
fn level_outcome_for_error(err: FetchError) -> PollOutcome {
    match err {
        FetchError::NoJwt => PollOutcome::SkippedNoJwt,
        FetchError::AuthOrAbsent(status) => PollOutcome::ResetOff(status),
        FetchError::Failed(e) => PollOutcome::Kept(e),
    }
}

/// One poll of the [`DOMAIN`] (install-interception) cache.
async fn poll_once() -> PollOutcome {
    match fetch_fleet_policy(DOMAIN).await {
        Ok(body) => PollOutcome::Updated(normalize_level(body.effective_level.as_deref())),
        Err(e) => level_outcome_for_error(e),
    }
}

/// One poll of the [`PLAN_CAPTURE_DOMAIN`] cache.
///
/// A 2xx whose `effective_level` is absent or unrecognised is
/// [`PollOutcome::Updated`] carrying `off`, NOT an error: coord answering
/// "nothing set" is a real answer, and the real answer is "do not instruct".
///
/// Note the response also carries `controls` / `drain` / `current_version`
/// blocks read from the unrelated `fleet_resources` row (coord's `read_controls`
/// answers with the tenant's controls whichever domain was asked for). Those are
/// NOT this domain's values and nothing here reads them.
async fn poll_plan_capture_once() -> PollOutcome {
    match fetch_fleet_policy(PLAN_CAPTURE_DOMAIN).await {
        Ok(body) => PollOutcome::Updated(normalize_plan_capture_level(
            body.effective_level.as_deref(),
        )),
        Err(e) => level_outcome_for_error(e),
    }
}

/// One poll of the [`CONTROLS_DOMAIN`] (session-protection floors) cache.
///
/// A 2xx with no usable floors — which is what coord returns until BOTH the
/// qontinui-web migration and coord's `CONTROL_COLS` widening land — is
/// [`ControlsOutcome::Updated`] carrying no floors, NOT an error. That
/// distinction is the fail-safe: `Updated(none)` publishes "the fleet has no
/// opinion", while an error arm would preserve floors that are no longer
/// current.
async fn poll_controls_once() -> ControlsOutcome {
    match fetch_fleet_policy(CONTROLS_DOMAIN).await {
        Ok(body) => ControlsOutcome::Updated(session_floors_from(&body)),
        Err(FetchError::NoJwt) => ControlsOutcome::SkippedNoJwt,
        Err(FetchError::AuthOrAbsent(status)) => ControlsOutcome::Reset(status),
        Err(FetchError::Failed(e)) => ControlsOutcome::Kept(e),
    }
}

// ===========================================================================
// Boot entry — mirrors device_jwt_refresher::commands
// ===========================================================================

/// Global state holder + public boot surface. Same shape as
/// `device_jwt_refresher::commands` so the `mcp_api` call sites read alike.
pub mod commands {
    use super::*;

    static POLLER_STATE: OnceLock<tokio::sync::Mutex<Option<Arc<PollerState>>>> = OnceLock::new();

    fn get_holder() -> &'static tokio::sync::Mutex<Option<Arc<PollerState>>> {
        POLLER_STATE.get_or_init(|| tokio::sync::Mutex::new(None))
    }

    /// Idempotent start. If a live task already exists, no-op (the poller has
    /// no kick — it's a fixed-cadence pull). If the prior task ended, restart.
    /// Wired beside `auto_start_device_jwt_refresher` in `mcp_api::start_server`
    /// — runs ONCE per runner (device-scoped), supervised, regardless of agents.
    pub async fn auto_start_fleet_policy_poller(api_state: Arc<ApiState>) {
        let mut guard = get_holder().lock().await;

        if let Some(ref existing) = *guard {
            let handle_guard = existing.task_handle.lock().await;
            let is_alive = handle_guard.as_ref().is_some_and(|h| !h.is_finished());
            drop(handle_guard);
            if is_alive {
                info!("Fleet-policy poller already running; leaving it");
                return;
            }
            info!("Fleet-policy poller task has ended, restarting...");
            existing.stop().await;
            *guard = None;
        }

        info!("Starting fleet-policy poller");
        let state = start_poller(api_state);
        *guard = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_constant_is_off() {
        // The fail-safe contract pin: the cache's resting/initial value is `off`
        // (NEVER gate before a successful poll). We assert the CONSTANT only —
        // NOT a live read of the shared process-global cache, which other
        // modules' tests legitimately mutate via `set_mode_for_test` and which
        // would race under cargo's parallel test threads.
        assert_eq!(DEFAULT_MODE, "off");
    }

    #[test]
    fn fresh_oncelock_initializes_to_off() {
        // Exercise the OnceLock init closure directly (a private throwaway lock,
        // not the shared global) so this is race-free: the cache MUST initialize
        // to the fail-safe default.
        let fresh = RwLock::new(DEFAULT_MODE.to_string());
        assert_eq!(*fresh.read().unwrap(), "off");
    }

    #[test]
    fn poll_interval_is_in_30_to_60s_window() {
        // Pin the cadence to the plan's window so a future "tune" has to update
        // this test and justify it in review.
        let s = POLL_INTERVAL.as_secs();
        assert!(
            (30..=60).contains(&s),
            "poll interval {s}s out of 30-60s window"
        );
    }

    #[test]
    fn unknown_level_is_normalized_off_via_response_shape() {
        // The decode + normalize path collapses an unrecognized level to off.
        // Calls the SHIPPING normalizer rather than a copy of it, so the test
        // cannot keep passing against logic the loop no longer runs.
        assert_eq!(normalize_level(Some("GATE")), "gate");
        assert_eq!(normalize_level(Some(" observe ")), "observe");
        assert_eq!(normalize_level(Some("bogus")), DEFAULT_MODE);
        assert_eq!(normalize_level(None), DEFAULT_MODE);
        assert_eq!(normalize_level(Some("")), DEFAULT_MODE);
    }

    #[test]
    fn capability_decode_disables_only_on_explicit_false() {
        // The §5 fail-safe contract: ONLY an explicit `fleet_policy:false`
        // disables the poller. Absent flag, no capabilities object, and a
        // decode of an older coord's body all ⇒ Capable (never break against a
        // coord that predates the field). We exercise the pure decision the
        // probe makes over the decoded `HealthResponse`.
        let decide = |json: &str| -> CapabilityCheck {
            let body: HealthResponse = serde_json::from_str(json).expect("decode");
            match body.capabilities {
                Some(caps) if caps.fleet_policy == Some(false) => CapabilityCheck::Disabled,
                _ => CapabilityCheck::Capable,
            }
        };

        // Explicit false ⇒ the ONLY disable case.
        assert_eq!(
            decide(r#"{"capabilities":{"fleet_policy":false}}"#),
            CapabilityCheck::Disabled
        );
        // Explicit true ⇒ capable.
        assert_eq!(
            decide(r#"{"capabilities":{"fleet_policy":true}}"#),
            CapabilityCheck::Capable
        );
        // capabilities present, fleet_policy absent (coord ships other caps but
        // not this flag) ⇒ capable.
        assert_eq!(
            decide(r#"{"capabilities":{"install_signatures":true}}"#),
            CapabilityCheck::Capable
        );
        // NO capabilities field at all (older coord predating the field) ⇒
        // capable — the defensive default that must never disable.
        assert_eq!(decide(r#"{"status":"ok"}"#), CapabilityCheck::Capable);
        // Empty body ⇒ capable.
        assert_eq!(decide(r#"{}"#), CapabilityCheck::Capable);
    }

    // =======================================================================
    // Session-protection floors (plan Part B item 3)
    // =======================================================================

    /// Decode helper: the tests speak in coord's wire JSON, not in the struct,
    /// because the whole risk being tested is what an unfamiliar body decodes
    /// to.
    fn floors_of(json: &str) -> SessionFloorsByLane {
        let body: FleetPolicyResponse = serde_json::from_str(json).expect("decode");
        session_floors_from(&body)
    }

    #[test]
    fn controls_domain_is_the_one_that_owns_the_columns() {
        // Pinned against coord's `fleet_policy::CONTROLS_DOMAIN`. If coord
        // renames the domain, the poller silently caches nothing — a test that
        // states the name is the cheapest place for that to be noticed.
        assert_eq!(CONTROLS_DOMAIN, "fleet_resources");
        // …and the interception domain is untouched by this generalization:
        // `install_effects_producer::run_with_base` reads its cache
        // synchronously and must keep working byte-for-byte.
        assert_eq!(DOMAIN, "install_interception");
    }

    #[test]
    fn fresh_floor_cache_holds_no_fleet_opinion() {
        // Exercise the OnceLock init value directly (a private throwaway, not
        // the shared global) so this is race-free. Before the first poll the
        // fleet term must contribute NOTHING on either lane.
        let fresh = SessionFloorsByLane::default();
        assert_eq!(fresh.host, SessionFloors::default());
        assert_eq!(fresh.wsl, SessionFloors::default());
        assert_eq!(fresh.host.warn_free_bytes, None);
        assert_eq!(fresh.host.critical_free_bytes, None);
    }

    #[test]
    fn all_four_floors_decode_onto_their_own_lanes() {
        // The lanes must not be crossed: the host warn floor is host-lane free
        // commit, the WSL warn floor is that VM's available memory, and a gate
        // judging one against the other is the failure mode
        // `fleet::resource_sample`'s docs exist to prevent.
        let floors = floors_of(
            r#"{"domain":"fleet_resources","controls_available":true,
                "controls":{"min_free_bytes_sessions_host":3221225472,
                            "min_free_bytes_sessions_wsl":2147483648,
                            "min_free_bytes_sessions_critical_host":1610612736,
                            "min_free_bytes_sessions_critical_wsl":1073741824}}"#,
        );
        assert_eq!(floors.host.warn_free_bytes, Some(3 * 1024 * 1024 * 1024));
        assert_eq!(
            floors.host.critical_free_bytes,
            Some(3 * 1024 * 1024 * 1024 / 2)
        );
        assert_eq!(floors.wsl.warn_free_bytes, Some(2 * 1024 * 1024 * 1024));
        assert_eq!(floors.wsl.critical_free_bytes, Some(1024 * 1024 * 1024));
    }

    /// THE PATH THAT RUNS TODAY. qontinui-web's `sess_guard_01` revision and
    /// coord's `CONTROL_COLS` widening are both still open, so coord answers
    /// with a `controls` object carrying the SIX older §D1 controls and none of
    /// the four floor columns. That must decode cleanly to "no fleet opinion" —
    /// not to a decode error (which would spam the log and keep stale floors)
    /// and not to zeros (which would name a floor of zero, disabling the guard).
    #[test]
    fn controls_without_the_floor_columns_yields_no_floors() {
        let floors = floors_of(
            r#"{"domain":"fleet_resources","effective_level":"off",
                "controls_available":true,
                "controls":{"min_free_mem_bytes_host":8589934592,
                            "min_free_mem_bytes_wsl":null,
                            "min_free_disk_bytes":21474836480,
                            "max_concurrent_builds_override":2,
                            "sample_interval_secs":30,
                            "sample_retention_days":14}}"#,
        );
        assert_eq!(floors, SessionFloorsByLane::default());
    }

    /// A partially-populated controls object contributes only what it stated.
    /// The unstated fields stay UNKNOWN rather than inheriting their sibling.
    #[test]
    fn a_partial_controls_object_contributes_only_what_it_states() {
        let floors = floors_of(
            r#"{"controls_available":true,
                "controls":{"min_free_bytes_sessions_host":4294967296}}"#,
        );
        assert_eq!(floors.host.warn_free_bytes, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(floors.host.critical_free_bytes, None);
        assert_eq!(floors.wsl, SessionFloors::default());
    }

    #[test]
    fn an_unprovisioned_or_absent_controls_object_yields_no_floors() {
        // Coord could not read the columns at all (pre-migration deployment).
        assert_eq!(
            floors_of(r#"{"controls_available":false,"controls":null}"#),
            SessionFloorsByLane::default()
        );
        // Explicit null with no flag.
        assert_eq!(
            floors_of(r#"{"controls":null}"#),
            SessionFloorsByLane::default()
        );
        // The key absent entirely (an older coord that predates §D1).
        assert_eq!(
            floors_of(r#"{"domain":"fleet_resources","effective_level":"off"}"#),
            SessionFloorsByLane::default()
        );
        // An empty body — the degenerate case that must still be harmless.
        assert_eq!(floors_of(r#"{}"#), SessionFloorsByLane::default());
        // `controls_available:false` WINS over a populated object: coord saying
        // it cannot read the columns is not a reading.
        assert_eq!(
            floors_of(
                r#"{"controls_available":false,
                    "controls":{"min_free_bytes_sessions_host":3221225472}}"#
            ),
            SessionFloorsByLane::default()
        );
    }

    /// A negative floor can only arrive from a hand-edited row (coord's
    /// `validate()` rejects it), and `as u64` would turn it into ~18 exabytes —
    /// a floor no machine can clear, blocking every spawn in the tenant. An
    /// impossible value is UNKNOWN.
    #[test]
    fn a_negative_floor_is_unknown_not_an_exabyte() {
        let floors = floors_of(
            r#"{"controls_available":true,
                "controls":{"min_free_bytes_sessions_host":-1,
                            "min_free_bytes_sessions_critical_host":-9223372036854775808}}"#,
        );
        assert_eq!(floors.host.warn_free_bytes, None);
        assert_eq!(floors.host.critical_free_bytes, None);
        assert_eq!(floor_bytes(Some(-1)), None);
        assert_eq!(floor_bytes(None), None);
        assert_eq!(
            floor_bytes(Some(i64::MAX)),
            Some(9_223_372_036_854_775_807_u64)
        );
    }

    /// Zero is preserved rather than collapsed to `None`: the fleet may state
    /// it, and stating it changes nothing, because the effective floor is a
    /// `max` that already contains the hardcoded default. Collapsing it would
    /// hide a real (if inert) fleet statement from the log line.
    #[test]
    fn a_zero_floor_is_kept_as_a_stated_zero() {
        let floors = floors_of(
            r#"{"controls_available":true,
                "controls":{"min_free_bytes_sessions_host":0}}"#,
        );
        assert_eq!(floors.host.warn_free_bytes, Some(0));
    }

    #[test]
    fn floors_are_selected_by_lane_and_an_unknown_lane_gets_none() {
        use crate::fleet::resource_sample::Lane;

        let floors = SessionFloorsByLane {
            host: SessionFloors {
                warn_free_bytes: Some(7),
                critical_free_bytes: Some(3),
            },
            wsl: SessionFloors {
                warn_free_bytes: Some(11),
                critical_free_bytes: None,
            },
        };
        assert_eq!(
            floors.for_lane(Lane::Host.as_str()).warn_free_bytes,
            Some(7)
        );
        assert_eq!(
            floors.for_lane(Lane::Wsl.as_str()).warn_free_bytes,
            Some(11)
        );
        // An unrecognised lane must get NO floors rather than a neighbouring
        // lane's — never compare one lane's reading to another lane's floor.
        assert_eq!(floors.for_lane("hyperv"), SessionFloors::default());
        assert_eq!(floors.for_lane(""), SessionFloors::default());
    }

    #[test]
    fn the_log_line_says_unset_never_zero() {
        // An operator reading the log must be able to tell "the fleet set no
        // floor" from "the fleet set a floor of 0" — printing both as `0` is
        // exactly the confusion the `Option` exists to prevent.
        let none = describe_floors(&SessionFloorsByLane::default());
        assert_eq!(
            none,
            "host warn=unset critical=unset, wsl warn=unset critical=unset"
        );

        let zeroed = SessionFloorsByLane {
            host: SessionFloors {
                warn_free_bytes: Some(0),
                critical_free_bytes: None,
            },
            wsl: SessionFloors::default(),
        };
        assert!(
            describe_floors(&zeroed).starts_with("host warn=0 critical=unset"),
            "a stated zero must print as 0: {}",
            describe_floors(&zeroed)
        );
    }

    // =======================================================================
    // Plan capture (2026-08-10-plan-and-prompt-library-in-web, Phase 4)
    // =======================================================================

    #[test]
    fn plan_capture_domain_and_default_are_pinned() {
        // The domain name is the whole contract with coord — it is opaque TEXT
        // there, so a typo produces a permanent 404 and a permanently `off`
        // clause with no error anywhere. State it.
        assert_eq!(PLAN_CAPTURE_DOMAIN, "plan_capture");
        assert_eq!(DEFAULT_PLAN_CAPTURE_LEVEL, "off");
        assert_eq!(PLAN_CAPTURE_RECORD, "record");
        // …and the two pre-existing domains are untouched by this addition.
        assert_eq!(DOMAIN, "install_interception");
        assert_eq!(CONTROLS_DOMAIN, "fleet_resources");
    }

    #[test]
    fn fresh_plan_capture_cache_initializes_to_off() {
        // Calls the SHIPPING init (`new_plan_capture_cache`, the very function
        // the OnceLock is seeded with) against a throwaway lock, so this is
        // race-free AND cannot keep passing if the real initial value moves.
        // This is the arm where a regression puts an unauthorized instruction
        // into every agent's system prompt on a runner that has never reached
        // coord, so it must exercise the shipping expression, not a copy.
        let fresh = new_plan_capture_cache();
        assert_eq!(*fresh.read().unwrap(), "off");
        // …and read back through the shipping reader too.
        assert_eq!(read_cached_level(&fresh, "sentinel-never-used"), "off");
    }

    #[test]
    fn only_the_exact_word_record_turns_plan_capture_on() {
        // Calls the SHIPPING normalizer, not a copy.
        assert_eq!(normalize_plan_capture_level(Some("record")), "record");
        assert_eq!(normalize_plan_capture_level(Some("RECORD")), "record");
        assert_eq!(normalize_plan_capture_level(Some("  Record ")), "record");
        // Explicit off.
        assert_eq!(normalize_plan_capture_level(Some("off")), "off");
        // The OTHER domain's vocabulary means nothing here — `gate` must not be
        // read as "an on-ish level, close enough".
        assert_eq!(normalize_plan_capture_level(Some("observe")), "off");
        assert_eq!(normalize_plan_capture_level(Some("gate")), "off");
        // Typos, empties and absence all collapse to off.
        assert_eq!(normalize_plan_capture_level(Some("recording")), "off");
        assert_eq!(normalize_plan_capture_level(Some("recor")), "off");
        assert_eq!(normalize_plan_capture_level(Some("")), "off");
        assert_eq!(normalize_plan_capture_level(Some("   ")), "off");
        assert_eq!(normalize_plan_capture_level(None), "off");
    }

    #[test]
    fn plan_capture_fail_safe_contract_holds_on_every_arm() {
        // The four arms of the D7 fail-safe contract, asserted against the
        // SHIPPING transition function the loop calls. `None` is the contract's
        // "cache untouched" — literally no write, not a last-good round trip.

        // 2xx with a level ⇒ that level, whichever way it moves.
        assert_eq!(
            next_plan_capture_level(&PollOutcome::Updated("record".into())),
            Some("record".to_string())
        );
        assert_eq!(
            next_plan_capture_level(&PollOutcome::Updated("off".into())),
            Some("off".to_string())
        );

        // Poll ERROR ⇒ NO write, so whatever the cache holds is kept.
        assert_eq!(
            next_plan_capture_level(&PollOutcome::Kept("request: timeout".into())),
            None
        );
        assert_eq!(
            next_plan_capture_level(&PollOutcome::Kept("decode: eof".into())),
            None
        );

        // 404 / 401 ⇒ an explicit write of off, EVEN FROM record. A tenant with
        // no row gets a 404, and that must read as "do not instruct" rather
        // than leave a stale `record` in place.
        assert_eq!(
            next_plan_capture_level(&PollOutcome::ResetOff(404)),
            Some("off".to_string())
        );
        assert_eq!(
            next_plan_capture_level(&PollOutcome::ResetOff(401)),
            Some("off".to_string())
        );

        // Unpaired ⇒ skipped quietly, cache untouched.
        assert_eq!(next_plan_capture_level(&PollOutcome::SkippedNoJwt), None);
    }

    #[test]
    fn a_2xx_with_no_level_is_an_answer_of_off_not_an_error() {
        // Coord answering "nothing set" is a real answer whose content is
        // `off` — decoded through the shipping wire type so a field rename
        // cannot keep this passing.
        let decode = |json: &str| -> String {
            let body: FleetPolicyResponse = serde_json::from_str(json).expect("decode");
            normalize_plan_capture_level(body.effective_level.as_deref())
        };
        assert_eq!(decode(r#"{"domain":"plan_capture"}"#), "off");
        assert_eq!(decode(r#"{"effective_level":null}"#), "off");
        assert_eq!(decode(r#"{"effective_level":"record"}"#), "record");
        // The cosmetic cross-domain blocks coord returns for this domain must
        // decode past us untouched and must NOT be read as plan_capture's own.
        assert_eq!(
            decode(
                r#"{"domain":"plan_capture","effective_level":"record",
                    "controls_available":true,
                    "controls":{"min_free_bytes_sessions_host":3221225472},
                    "current_version":7}"#
            ),
            "record"
        );
    }

    #[test]
    fn a_fetch_failure_classifies_the_same_way_for_both_level_domains() {
        // One decision, shared: the two level caches may hold different values
        // but must never disagree about what a 404 means.
        assert_eq!(
            level_outcome_for_error(FetchError::NoJwt),
            PollOutcome::SkippedNoJwt
        );
        assert_eq!(
            level_outcome_for_error(FetchError::AuthOrAbsent(404)),
            PollOutcome::ResetOff(404)
        );
        assert_eq!(
            level_outcome_for_error(FetchError::AuthOrAbsent(401)),
            PollOutcome::ResetOff(401)
        );
        assert_eq!(
            level_outcome_for_error(FetchError::Failed("request: dns".into())),
            PollOutcome::Kept("request: dns".into())
        );
    }

    #[test]
    fn a_poisoned_plan_capture_lock_degrades_to_off() {
        // A poisoned lock must read as `off` (no clause), not panic and not
        // hold a stale `record`. Exercised through the SHIPPING reader against
        // a throwaway lock, so the shared global is not disturbed.
        let lock = RwLock::new(PLAN_CAPTURE_RECORD.to_string());
        assert_eq!(
            read_cached_level(&lock, DEFAULT_PLAN_CAPTURE_LEVEL),
            "record"
        );

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock.write().unwrap();
            panic!("poison the lock");
        }));
        assert!(lock.is_poisoned(), "the lock must actually be poisoned");
        assert_eq!(read_cached_level(&lock, DEFAULT_PLAN_CAPTURE_LEVEL), "off");
    }

    /// Replays a tick sequence through the SHIPPING edge-trigger + key and
    /// counts the log lines it would emit.
    fn logged_lines(ticks: &[PollOutcome]) -> usize {
        let mut last: Option<String> = None;
        let mut lines = 0;
        for outcome in ticks {
            let key = plan_capture_log_key(outcome);
            if is_new_outcome(last.as_ref(), &key) {
                lines += 1;
                last = Some(key);
            }
        }
        lines
    }

    #[test]
    fn degradation_is_logged_once_per_transition_not_once_per_tick() {
        // Five ticks, two states: unpaired ×3 then 404 ×2 ⇒ two lines.
        assert_eq!(
            logged_lines(&[
                PollOutcome::SkippedNoJwt,
                PollOutcome::SkippedNoJwt,
                PollOutcome::SkippedNoJwt,
                PollOutcome::ResetOff(404),
                PollOutcome::ResetOff(404),
            ]),
            2,
            "one line per transition, not one per tick"
        );

        // THE ARM A WHOLE-VALUE COMPARISON GETS WRONG. A sustained network
        // failure whose message varies tick to tick (reqwest embeds the DNS
        // detail, the OS error number, the ephemeral port) is a different
        // `Kept(String)` every time — so comparing outcomes by equality would
        // log every 45s under exactly the sustained-failure conditions the
        // once-per-transition rule exists to keep quiet.
        assert_eq!(
            logged_lines(&[
                PollOutcome::Kept("request: dns error 11001".into()),
                PollOutcome::Kept("request: connect os error 10060".into()),
                PollOutcome::Kept("request: connect 127.0.0.1:51922 refused".into()),
            ]),
            1,
            "a sustained failure with varying detail is ONE degradation, not three"
        );

        // …but the things an operator must see still open a new line: a level
        // change, and a different status.
        assert_eq!(
            logged_lines(&[
                PollOutcome::Updated("off".into()),
                PollOutcome::Updated("off".into()),
                PollOutcome::Updated("record".into()),
                PollOutcome::Updated("record".into()),
                PollOutcome::Updated("off".into()),
            ]),
            3
        );
        assert_eq!(
            logged_lines(&[PollOutcome::ResetOff(404), PollOutcome::ResetOff(401)]),
            2
        );
        // Recovering out of a failure into a level is a transition too.
        assert_eq!(
            logged_lines(&[
                PollOutcome::Kept("request: timeout".into()),
                PollOutcome::Updated("record".into()),
            ]),
            2
        );

        // `None` (nothing logged yet) is always a transition.
        assert!(is_new_outcome(None::<&String>, &"skipped".to_string()));
    }
}
