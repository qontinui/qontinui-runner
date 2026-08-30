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
//! This module owns FOUR process-global caches and one supervised background
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
//! 4. The tenant's **session-briefing documents** — coord prompt documents
//!    under kind [`BRIEFING_KIND`], read synchronously by
//!    [`crate::terminal::runner_context`] and by [`crate::mcp::ai_session`],
//!    which are the two places the runner injects a prompt of its own into a
//!    session. Unlike the three above, this cache is **persisted to disk** (see
//!    below) and is refreshed by ONE conditional LIST request per tick plus a
//!    body fetch only for a document whose `current_version` actually moved —
//!    coord implements `If-None-Match` on the list route only, so a steady
//!    state costs one 304 and nothing else. Plan
//!    `2026-08-20-runner-session-briefing-versioned-and-operator-editable`.
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
//!
//! For the session-briefing documents, the SAME posture with the compiled-in
//! **builtin** playing the safe role — it is the text the runner injected
//! before this cache existed, so falling back to it cannot make the runner do
//! anything new:
//!
//! - Before the FIRST successful poll ⇒ the **persisted last-good** if the
//!   store file has one, else the **builtin**. The persisted body is labelled
//!   `cached … (stale)` on the provenance line until a poll confirms it — the
//!   runner never claims `coord` for text it has not checked in this process.
//! - A poll ERROR (network / decode / a 2xx with no body) ⇒ **last-good**.
//! - A document coord's LIST does not serve this device ⇒ its cached body is
//!   DROPPED and the **builtin** renders.
//! - A coord **404 / 403** ⇒ the cached body is DROPPED and the **builtin**
//!   renders. The document is absent, or it is not this device's to read;
//!   either way the runner must not serve a guessed or another tenant's text.
//!   404 is the normal answer until coord's half of the plan ships, so it logs
//!   at `info`.
//! - A coord **401** ⇒ **last-good**, NOT a reset. 401 is a statement about
//!   the CREDENTIAL, not the document: device JWTs live ~4h and the token check
//!   before the fetch only proves a bearer exists. Resetting on it would let a
//!   token blip rewrite the on-disk store empty and permanently lose the
//!   tenant's edited briefing.
//! - Unpaired ⇒ SKIPPED quietly, cache untouched.
//! - A **poisoned lock** ⇒ the reader returns `None` ⇒ **builtin**.
//! - A body that fails the RENDER-TIME guard
//!   ([`crate::mcp::session_briefing::validate_body`]) ⇒ **builtin**, labelled
//!   `builtin-fallback (rejected coord v<N>)`. Coord validates the same things
//!   at write time; this end is what keeps a structural guarantee in the
//!   process that actually builds the prompt.
//! - Degradation is logged ONCE, on a transition, PER DOCUMENT.
//!
//! **Disk persistence is PER RUNNER INSTANCE.** The store lives in the
//! directory [`crate::settings::resolve_config_dir`] names — the NON-creating
//! resolver, because every door into the cache is a read (see
//! [`briefing_store_path`]) — which honours `$QONTINUI_CONFIG_DIR`, a variable
//! the supervisor sets per spawned runner. So "a restart does not
//! silently revert a tenant's edited briefing" holds for a given instance, and
//! a freshly spawned temp/secondary runner legitimately starts on the builtin
//! until its first poll. It is not machine-wide state.

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
/// `pub(crate)` because [`crate::terminal::runner_context`] compares the cached
/// level against it to decide whether to append the capture clause. The word is
/// the contract between the normalizer that WRITES the cache and the briefing
/// that READS it, so it has exactly one definition rather than a matching pair
/// that a rename could silently split.
pub(crate) const PLAN_CAPTURE_RECORD: &str = "record";

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
    /// Warn CEILING on the runner process's OS thread count — above this,
    /// starting another session is discouraged but allowed.
    ///
    /// **Currently DORMANT: coord publishes no thread column, so this is always
    /// `None`.** Nothing coord-side was changed to land it; the wire type below
    /// is a permissive response subset whose every field is an
    /// `Option` with a `#[serde(default)]`, so the two column names decode to
    /// `None` against today's payload and would start carrying a value the day
    /// coord adds them, with no runner change. Until then the thread lane's
    /// effective ceiling folds to `min(local, hardcoded)` — exactly the
    /// fail-safe this poller documents for every other term it caches.
    ///
    /// `u32` rather than `u64`: a thread count that needs more than 32 bits is
    /// not a configuration, it is a decode error. The narrowing happens once,
    /// in [`thread_ceiling`].
    pub(crate) warn_thread_count: Option<u32>,
    /// Critical CEILING on the runner process's OS thread count — above this a
    /// new spawn is refused by default. Dormant for the same reason as
    /// [`Self::warn_thread_count`].
    pub(crate) critical_thread_count: Option<u32>,
}

/// Both lanes' floors, as one cached value so a poll swaps them atomically and
/// a reader can never see the host lane from one poll beside the WSL lane from
/// another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SessionFloorsByLane {
    host: SessionFloors,
    wsl: SessionFloors,
    /// The process-wide thread lane. It carries ONLY the two thread fields —
    /// there is no such thing as "free commit on the threads lane" — and the
    /// two memory lanes carry only the two byte fields, for the mirror reason.
    /// One struct for all three because a lane's limits are looked up by name
    /// through [`SessionFloorsByLane::for_lane`], and a per-lane type would
    /// make that lookup return three different things.
    threads: SessionFloors,
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
        } else if lane == Lane::Threads.as_str() {
            self.threads
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
/// `pub(crate)` rather than `pub`: the level is only meaningful next to
/// [`PLAN_CAPTURE_RECORD`], which is also crate-visible, so exporting the
/// reader without the vocabulary would hand a caller a string it cannot
/// correctly compare.
pub(crate) fn effective_plan_capture_level() -> String {
    read_cached_level(plan_capture_cache(), DEFAULT_PLAN_CAPTURE_LEVEL)
}

/// Overwrite the cached plan-capture level. Internal — only the poll loop calls
/// this (and the test guard below).
fn set_plan_capture_level(level: &str) {
    if let Ok(mut g) = plan_capture_cache().write() {
        *g = level.to_string();
    }
}

/// Test-only RAII pin over EVERY process-global this module publishes onto the
/// spawn path: the plan-capture level and the session-briefing cache.
///
/// Two jobs, both of which a bare setter got wrong:
///
/// 1. It SERIALIZES. The state is process-global, so two tests pinning it
///    concurrently under cargo's thread pool would decide each other's outcome.
///    The mutex lives HERE, beside the caches, so every module that pins them
///    shares one — a mutex private to `terminal::tests` cannot be taken by
///    `agent_runtime`'s briefing test, which renders the same string.
/// 2. It RESTORES on `Drop`, so a FAILING assertion (which panics past any
///    trailing restore statement) cannot leak `record` — or somebody's planted
///    briefing body — into the next test and turn one real failure into a
///    cascade. Poisoning is deliberately absorbed for the same reason: the
///    guard's whole point is that the next test starts from the fail-safe
///    values whatever happened to the last one.
///
/// The BRIEFING cache is covered by the same guard rather than a second one on
/// purpose: `runner_context` reads both in a single render, so two independent
/// locks would let one test's level sit beside another test's briefing.
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

    /// Plant one document in the process-global briefing cache.
    pub(crate) fn set_briefing(&self, name: &str, doc: BriefingDocument) {
        if let Ok(mut g) = briefing_cache().write() {
            g.insert(name.to_string(), doc);
        }
    }
}

#[cfg(test)]
impl Drop for PlanCaptureLevelPin {
    fn drop(&mut self) {
        set_plan_capture_level(DEFAULT_PLAN_CAPTURE_LEVEL);
        clear_briefing_cache();
        // The list validator is process-global too, and a test that plants one
        // and then fails an assertion would otherwise leak it into the next.
        set_briefing_list_etag(None);
    }
}

/// Reset the briefing cache to its fail-safe value: EMPTY, i.e. every render
/// falls back to the compiled-in builtin.
#[cfg(test)]
fn clear_briefing_cache() {
    if let Ok(mut g) = briefing_cache().write() {
        g.clear();
    }
}

/// Build a [`BriefingDocument`] for a test without spelling every field at each
/// call site.
#[cfg(test)]
pub(crate) fn briefing_for_test(
    body: &str,
    version: i64,
    provenance: BriefingProvenance,
) -> BriefingDocument {
    BriefingDocument {
        body: body.to_string(),
        version,
        fetched_at: "2026-08-20T00:00:00+00:00".to_string(),
        provenance,
    }
}

/// Acquire the pin. Blocks until any other pinning test has released it, and
/// restores the fail-safe level AND the empty briefing cache when the returned
/// guard drops.
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
// Process-global cache #4 — the operator-editable session-briefing documents
// ===========================================================================

/// Coord prompt-document KIND carrying the runner-injected session prompts
/// (plan `2026-08-20-runner-session-briefing-versioned-and-operator-editable`).
pub(crate) const BRIEFING_KIND: &str = "session_briefing";

/// The base briefing [`crate::terminal::runner_context`] renders.
pub(crate) const BRIEFING_RUNNER_SESSION: &str = "runner-session";

/// The fleet-gated plan-capture clause appended to the base briefing. It moves
/// with the briefing because an operator who can edit one but not the other
/// cannot see the whole prompt.
pub(crate) const BRIEFING_PLAN_CAPTURE_CLAUSE: &str = "plan-capture-clause";

/// The SECOND runner-injected prompt, rendered by [`crate::mcp::ai_session`] on
/// a spawn seam `runner_context` never reaches.
pub(crate) const BRIEFING_AI_SESSION_RULES: &str = "ai-session-rules";

/// The three fixed names this runner reads under [`BRIEFING_KIND`]. Extra rows
/// an operator creates under this kind are INERT — the names are constants on
/// both sides.
pub(crate) const BRIEFING_NAMES: [&str; 3] = [
    BRIEFING_RUNNER_SESSION,
    BRIEFING_PLAN_CAPTURE_CLAUSE,
    BRIEFING_AI_SESSION_RULES,
];

/// The on-disk last-good store, under the directory
/// [`crate::settings::resolve_config_dir`] names — see [`briefing_store_path`]
/// for why it is the non-creating resolver.
const BRIEFING_STORE_FILE: &str = "session-briefing.cache.json";

/// How far a cached body has travelled from coord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BriefingProvenance {
    /// Fetched or re-confirmed (304) by a live poll in THIS process.
    Coord,
    /// Restored from [`BRIEFING_STORE_FILE`] and not yet re-confirmed — the
    /// runner may be offline, unpaired, or simply not have ticked yet.
    Cached,
}

/// A cached `session_briefing` document.
///
/// Serialized as-is into the disk store, so every field carries
/// `#[serde(default)]`: a store written by an older build must degrade to a
/// usable value rather than failing the whole restore.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct BriefingDocument {
    /// The RAW body as coord served it — placeholders unsubstituted and
    /// UNVALIDATED. The render-time guard
    /// ([`crate::mcp::session_briefing::validate_body`]) runs on the way OUT,
    /// not on the way in, so a body planted directly in the store file is
    /// refused at exactly the point where the prompt is built.
    #[serde(default)]
    pub(crate) body: String,
    /// The row's `current_version`, as it appears on the provenance line, and
    /// the value the per-tick version gate compares against — a body is only
    /// re-fetched when the LIST says this number moved.
    #[serde(default)]
    pub(crate) version: i64,
    /// When the body was last confirmed against coord (RFC 3339).
    #[serde(default)]
    pub(crate) fetched_at: String,
    #[serde(default = "default_briefing_provenance")]
    pub(crate) provenance: BriefingProvenance,
}

/// A restored store is `cached` by construction — the process has not yet
/// spoken to coord about it, and saying `coord` would be a claim the runner
/// cannot back.
fn default_briefing_provenance() -> BriefingProvenance {
    BriefingProvenance::Cached
}

type BriefingCache = std::collections::HashMap<String, BriefingDocument>;

static SESSION_BRIEFINGS: OnceLock<RwLock<BriefingCache>> = OnceLock::new();

fn briefing_cache() -> &'static RwLock<BriefingCache> {
    SESSION_BRIEFINGS.get_or_init(|| RwLock::new(initial_briefing_cache()))
}

/// The cache's INITIAL value: the persisted last-good, if any.
///
/// Loading in the `OnceLock` initializer rather than from an explicit boot call
/// is deliberate — it makes "loaded BEFORE the first session can spawn"
/// structural rather than dependent on where someone wired the call. The read
/// is one small file, once per process.
#[cfg(not(test))]
fn initial_briefing_cache() -> BriefingCache {
    load_persisted_briefings()
}

/// Test builds never touch the real config dir (the `helper_tasks` posture).
#[cfg(test)]
fn initial_briefing_cache() -> BriefingCache {
    BriefingCache::new()
}

/// Read one cached `session_briefing` document.
///
/// SYNCHRONOUS + lock-only, like [`effective_plan_capture_level`] — this is
/// called from [`crate::terminal::runner_context`] on the spawn path. A
/// poisoned lock, or an absent document, both yield `None`, which the renderer
/// turns into the compiled-in builtin.
pub(crate) fn cached_briefing(name: &str) -> Option<BriefingDocument> {
    briefing_cache()
        .read()
        .ok()
        .and_then(|g| g.get(name).cloned())
}

/// Every cached document, for persistence. (The visibility route does NOT
/// use this — it takes its text from `runner_context()` itself.)
fn briefing_snapshot() -> BriefingCache {
    briefing_cache()
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

// ===========================================================================
// Layer 10 of the config report — the dial, as it stands RIGHT NOW.
//
// Plan `2026-08-20-effective-config-provenance-and-env-generation` Phase 4.
// ===========================================================================

/// One cached `session_briefing` document, reduced to what a report may say.
///
/// The BODY is deliberately absent. It is operator-authored prose that lands
/// verbatim in a system prompt, it can be arbitrarily long, and none of it is
/// needed to answer "which generation of the briefing is this session getting?"
/// — which is `version` + `fetched_at` + `provenance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BriefingDial {
    /// One of [`BRIEFING_NAMES`].
    pub(crate) name: &'static str,
    /// `false` ⇒ no cached document under this name, so the renderer falls back
    /// to the compiled-in builtin. That is a reading about the CACHE, not a
    /// statement that coord has no such document.
    pub(crate) present: bool,
    /// The row's `current_version` as the cache holds it. `None` when the cache
    /// holds no document under this name AND when it holds one whose version is
    /// `0` — the value a list row with no `current_version` and an older
    /// build's store entry both decode to, which the version gate below already
    /// refuses to read as a generation number.
    pub(crate) version: Option<i64>,
    /// **The cache's own last-refresh time** (RFC 3339), as coord confirmed it.
    /// This is the sub-fact caches 1-3 cannot supply — see
    /// [`FleetPolicyDial::caches_expose_refresh_time`].
    pub(crate) fetched_at: Option<String>,
    /// `coord` (confirmed by a live poll in this process) vs `cached` (restored
    /// from the on-disk last-good and not yet re-confirmed).
    pub(crate) provenance: Option<&'static str>,
}

/// Everything the fleet-policy dial currently holds, in one atomic-enough read.
///
/// # The freshness asymmetry this type exists to make visible
///
/// Four process-global caches sit behind ONE poll loop, and they do NOT agree
/// about what they can tell you:
///
/// - Caches 1-3 (interception mode, session floors, plan-capture level) are a
///   bare `RwLock<T>`. They hold a VALUE and nothing else — no stamp, no
///   generation counter. So "when did this last change?" is genuinely
///   unanswerable from here, and [`caches_expose_refresh_time`](Self::caches_expose_refresh_time)
///   says `false` rather than letting a consumer substitute the read time. A
///   read time presented as a refresh time would make every reading look
///   perfectly fresh, which is the precise lie a time-varying layer's row
///   exists to prevent.
/// - Cache 4 (the session briefings) DOES carry `fetched_at` + `provenance`
///   per document, because it is persisted across restarts and a restored
///   last-good must be distinguishable from a live confirmation.
///
/// Reporting both halves under one honest shape is why this is a struct and
/// not four getters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FleetPolicyDial {
    /// [`POLL_INTERVAL`] in milliseconds — the upper bound on how stale any of
    /// these values can be while the loop is running.
    pub(crate) poll_interval_ms: u128,
    /// Cache 1 — the effective install-interception mode.
    pub(crate) install_intercept_mode: String,
    /// Cache 1's resting value. Equal to `install_intercept_mode` means EITHER
    /// "the fleet says off" OR "no poll has ever succeeded", and the two are
    /// indistinguishable from this cache — which is itself worth reporting.
    pub(crate) install_intercept_default: &'static str,
    /// Cache 2 — the host lane's warn floor, in bytes. `None` = the fleet has
    /// no opinion (never `0`, which would disable the guard it names).
    pub(crate) host_warn_free_bytes: Option<u64>,
    /// Cache 2 — the host lane's critical floor, in bytes.
    pub(crate) host_critical_free_bytes: Option<u64>,
    /// Cache 2 — the WSL lane's warn floor, in bytes. NEVER interchangeable
    /// with the host lane's.
    pub(crate) wsl_warn_free_bytes: Option<u64>,
    /// Cache 2 — the WSL lane's critical floor, in bytes.
    pub(crate) wsl_critical_free_bytes: Option<u64>,
    /// Cache 2 — the thread lane's warn CEILING, in OS threads. `None` = the
    /// fleet has no opinion, which today is the ONLY value it can hold: no
    /// coord column publishes a thread ceiling yet (see
    /// [`SessionFloors::warn_thread_count`]). Reported anyway, because a dial
    /// that silently omits a cached term cannot be used to tell "the fleet said
    /// nothing" from "the runner never looked".
    pub(crate) threads_warn_count: Option<u32>,
    /// Cache 2 — the thread lane's critical CEILING, in OS threads.
    pub(crate) threads_critical_count: Option<u32>,
    /// Cache 3 — the plan-capture level (`off` | `record`).
    pub(crate) plan_capture_level: String,
    /// Cache 3's resting value.
    pub(crate) plan_capture_default: &'static str,
    /// Cache 3's ON word, so a reader can tell whether the level is armed
    /// without knowing the vocabulary.
    pub(crate) plan_capture_record_level: &'static str,
    /// Cache 4 — one entry per name in [`BRIEFING_NAMES`], always all three
    /// (an absent document is `present: false`, never a missing entry).
    pub(crate) briefings: Vec<BriefingDial>,
    /// `false`, always, for caches 1-3 — see the type docs. Kept as a FIELD
    /// rather than a comment so the consumer's honesty is data-driven: a future
    /// stamp on those caches flips this and the report gains the fact without
    /// the consumer inventing one in the meantime.
    pub(crate) caches_expose_refresh_time: bool,
}

/// Snapshot every cache this poller owns, WITHOUT polling.
///
/// Lock-only and side-effect-free, exactly like the four production readers —
/// a config report must never be what makes a dial move, or the act of
/// observing changes the answer.
///
/// Not atomic across the four locks, and deliberately not made so: the poll
/// loop writes them within microseconds of each other once per
/// [`POLL_INTERVAL`], and taking a cross-cache lock here would put the config
/// report on the spawn path's critical section to buy a guarantee no reader of
/// the report can use.
pub(crate) fn dial_snapshot() -> FleetPolicyDial {
    use crate::fleet::resource_sample::Lane;

    let host = fleet_session_floors(Lane::Host.as_str());
    let wsl = fleet_session_floors(Lane::Wsl.as_str());
    let threads = fleet_session_floors(Lane::Threads.as_str());
    let cache = briefing_snapshot();

    FleetPolicyDial {
        poll_interval_ms: POLL_INTERVAL.as_millis(),
        install_intercept_mode: effective_install_intercept_mode(),
        install_intercept_default: DEFAULT_MODE,
        host_warn_free_bytes: host.warn_free_bytes,
        host_critical_free_bytes: host.critical_free_bytes,
        wsl_warn_free_bytes: wsl.warn_free_bytes,
        wsl_critical_free_bytes: wsl.critical_free_bytes,
        threads_warn_count: threads.warn_thread_count,
        threads_critical_count: threads.critical_thread_count,
        plan_capture_level: effective_plan_capture_level(),
        plan_capture_default: DEFAULT_PLAN_CAPTURE_LEVEL,
        plan_capture_record_level: PLAN_CAPTURE_RECORD,
        briefings: BRIEFING_NAMES
            .iter()
            .map(|name| match cache.get(*name) {
                Some(doc) => BriefingDial {
                    name,
                    present: true,
                    // `0` is UNKNOWN, not a generation — the same rule the
                    // version gate below enforces, and the reason
                    // `config_report_cmd` renders `None` as `v?`. Reported
                    // verbatim it printed `runner-session=v0`, which made that
                    // arm unreachable and stated a version the runner does not
                    // have.
                    version: Some(doc.version).filter(|v| *v != 0),
                    // Empty string is the serde default for a store written by
                    // a build that predates the field — report that as UNKNOWN
                    // rather than as an empty timestamp.
                    fetched_at: Some(doc.fetched_at.clone()).filter(|s| !s.is_empty()),
                    provenance: Some(match doc.provenance {
                        BriefingProvenance::Coord => "coord",
                        BriefingProvenance::Cached => "cached",
                    }),
                },
                None => BriefingDial {
                    name,
                    present: false,
                    version: None,
                    fetched_at: None,
                    provenance: None,
                },
            })
            .collect(),
        caches_expose_refresh_time: false,
    }
}

// ---- disk persistence (the `helper_tasks::store_path` posture) -------------

/// The last-good store path, resolved **without creating anything**.
///
/// [`crate::settings::resolve_config_dir`] honours `$QONTINUI_CONFIG_DIR`, which
/// the supervisor sets PER RUNNER INSTANCE. So "a restart does not revert the
/// tenant's edited briefing" is true per instance, and a freshly spawned
/// temp/secondary runner legitimately starts on the builtin until its first
/// poll. This is NOT machine-wide persistence, and `dirs::config_dir()` is
/// deliberately not used (`claude_accounts.rs:92-97` documents that as the
/// unscoped path).
///
/// # Why the NON-creating resolver, and why that matters here of all places
///
/// This function used to call `settings`' ensuring variant, whose doc says in
/// so many words *"Never call this from a read or a diagnostic"* — and this is
/// reached from a READ. [`briefing_cache`] initialises the `OnceLock` from
/// [`load_persisted_briefings`], so whichever caller touches the cache FIRST in
/// a process pays the resolution, and `config_report`'s layer 11 is one of them
/// (`config_report_cmd` → `dial_snapshot` → `briefing_snapshot` →
/// [`briefing_cache`]), as is its env-generation section by a second route
/// (`pty_child_command` → `apply_base_child_env` → `terminal::runner_context` →
/// [`cached_briefing`]). Both run BEFORE the report stats the config directory,
/// so a runner launched with a typo'd `QONTINUI_CONFIG_DIR` had that directory
/// brought into existence by the report, which then printed `on disk: true` —
/// the report materializing the fault it was opened to explain. That is the
/// same defect `settings::resolve_config_dir`'s own F5 regression test closed
/// on layer 2, arriving through a third door.
///
/// Non-creation is the invariant, so the WRITER creates the directory
/// explicitly at its own call site ([`persist_briefings`]) rather than as a
/// side effect of asking where the file is.
fn briefing_store_path() -> Option<std::path::PathBuf> {
    crate::settings::resolve_config_dir()
        .ok()
        .map(|(dir, _config_dir_source)| dir.join(BRIEFING_STORE_FILE))
}

/// Restore the persisted last-good briefings. Best-effort: a missing file is
/// first boot, a corrupt file is logged and ignored. The decode rules live in
/// [`restore_briefings_from_str`].
#[cfg(not(test))]
fn load_persisted_briefings() -> BriefingCache {
    let Some(path) = briefing_store_path() else {
        return BriefingCache::new();
    };
    load_persisted_briefings_from(&path)
}

/// [`load_persisted_briefings`] against an EXPLICIT path.
///
/// Compiled in both cfgs, and the reason is the guard above: the `#[cfg(test)]`
/// twin of [`initial_briefing_cache`] means no test build ever reaches the real
/// store, so "the read path creates nothing" was a claim nothing could check.
/// Pointing this half at a root the test owns makes it checkable without
/// mutating `$QONTINUI_CONFIG_DIR` out from under every sibling test.
///
/// It READS ONLY. A missing file — and a missing parent DIRECTORY — are both
/// "first boot", never a reason to create either.
fn load_persisted_briefings_from(path: &std::path::Path) -> BriefingCache {
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(_) => return BriefingCache::new(), // first boot
    };
    let restored = restore_briefings_from_str(&raw);
    if !restored.is_empty() {
        info!(
            "session_briefing: restored {} persisted document(s) from {} \
             (labelled `cached` until the first poll confirms them)",
            restored.len(),
            path.display()
        );
    }
    restored
}

/// The PURE half of the restore: decode, drop unknown names, force-relabel.
///
/// Compiled in both cfgs so the interesting logic is testable — a test build
/// never reads the real config dir, but the rules below are exactly where a
/// planted or drifted store file would do damage.
///
/// Unknown document names are DROPPED: the three names are a closed contract,
/// and a store carrying a fourth is another build's or a planted file. Every
/// restored entry is marked [`BriefingProvenance::Cached`] regardless of what
/// the file claims, so a hand-edited `"provenance":"coord"` cannot buy a body a
/// label it has not earned in THIS process. A corrupt file is logged and
/// ignored — never a panic, never a partial restore.
fn restore_briefings_from_str(raw: &str) -> BriefingCache {
    match serde_json::from_str::<BriefingCache>(raw) {
        Ok(loaded) => loaded
            .into_iter()
            .filter(|(name, _)| BRIEFING_NAMES.contains(&name.as_str()))
            .map(|(name, mut doc)| {
                doc.provenance = BriefingProvenance::Cached;
                (name, doc)
            })
            .collect(),
        Err(e) => {
            warn!("session_briefing: persisted store unreadable — starting on builtin: {e}");
            BriefingCache::new()
        }
    }
}

/// Persist the current cache. Best-effort — a write failure only costs
/// durability across the next restart, never the in-memory state.
///
/// Called only when CONTENT changed, never on every 304: the store is up to
/// three 16 KiB bodies and rewriting it every 45s forever would be pure churn.
///
/// This is the ONLY place in this module allowed to create the config
/// directory, and it does so with an explicit `create_dir_all` rather than by
/// asking an ensuring resolver where the file lives — see
/// [`briefing_store_path`] for why that distinction is load-bearing.
/// `fs_atomic::atomic_write` writes its temp file into the parent, so the
/// parent has to exist first; a create failure is best-effort like every other
/// failure on this path.
#[cfg(not(test))]
fn persist_briefings() {
    let Some(path) = briefing_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("session_briefing: store dir create failed (best-effort): {e}");
            return;
        }
    }
    let snapshot = briefing_snapshot();
    let body = match serde_json::to_vec(&snapshot) {
        Ok(b) => b,
        Err(e) => {
            warn!("session_briefing: store serialize failed (best-effort): {e}");
            return;
        }
    };
    if let Err(e) = crate::fs_atomic::atomic_write(&path, &body) {
        warn!("session_briefing: store persist failed (best-effort): {e}");
    }
}

/// Test builds never touch the real config dir.
#[cfg(test)]
fn persist_briefings() {}

// ---- fetch + the fail-safe contract ---------------------------------------
//
// ## Why the conditional request is on the LIST route, not on each document
//
// The obvious shape — three conditional `GET
// /coord/agent-prompt-documents/session_briefing/<name>` with `If-None-Match`,
// for three 304s per tick — CANNOT WORK against coord as it stands. Only the
// LIST route implements the validator: coord's `get_agent_list` takes a
// `HeaderMap` and `list_response` compares `If-None-Match`, while
// `get_agent_one` takes no headers and `one_response` emits no `ETag`
// (`crates/coord/src/prompt_documents.rs`, verified on `origin/main`). A
// conditional single-document GET would therefore degrade SILENTLY to an
// unconditional body fetch — three full bodies every 45s forever, with the
// header doing nothing at all.
//
// So the tick is:
//
// 1. ONE conditional `GET …?kind=session_briefing`. Steady state is one 304.
// 2. On a 200, the list carries `current_version` per document but no bodies
//    (coord's `SUMMARY_COLS`). Fetch a body ONLY for a document whose version
//    actually moved.
//
// That is strictly fewer requests than the per-document form would have made
// even if it had worked, and it needs no coord change.

/// The agent-door LIST url, filtered to this kind. Coord scopes the rows to the
/// caller's tenant from the bearer — never pass a tenant here.
fn briefing_list_url(base: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents?kind={BRIEFING_KIND}",
        base.trim_end_matches('/')
    )
}

/// The agent-door SINGLE-document URL.
///
/// `/coord/agent-prompt-documents/*` is the DEVICE door. Its operator sibling
/// `/coord/prompt-documents/*` 403s a device JWT, which is why the render-time
/// guard also refuses a body that names it.
fn briefing_url(base: &str, name: &str) -> String {
    format!(
        "{}/coord/agent-prompt-documents/{BRIEFING_KIND}/{}",
        base.trim_end_matches('/'),
        urlencoding::encode(name)
    )
}

/// The LIST response's `ETag`, replayed as `If-None-Match`.
///
/// Process-global beside the document cache rather than a field on it: it
/// validates the SET of documents, not any one body, and pinning it to one
/// entry would make its lifetime depend on which document happened to be
/// cached.
static BRIEFING_LIST_ETAG: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn briefing_list_etag_cell() -> &'static RwLock<Option<String>> {
    BRIEFING_LIST_ETAG.get_or_init(|| RwLock::new(None))
}

fn briefing_list_etag() -> Option<String> {
    briefing_list_etag_cell()
        .read()
        .ok()
        .and_then(|g| g.clone())
}

fn set_briefing_list_etag(etag: Option<String>) {
    if let Ok(mut g) = briefing_list_etag_cell().write() {
        *g = etag;
    }
}

/// Outcome of the once-per-tick conditional LIST fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BriefingListOutcome {
    /// 2xx — `name → current_version` for every `session_briefing` row coord
    /// serves this device, plus the response validator.
    Updated {
        versions: std::collections::HashMap<String, i64>,
        etag: Option<String>,
    },
    /// 304 — the whole set is unchanged, so every cached body is CONFIRMED
    /// current (a cached body is only ever stored at a listed version).
    NotModified,
    /// No device JWT (unpaired) — skipped quietly, cache untouched.
    SkippedNoJwt,
    /// 401 / 403 / 404 — the kind is absent or this device is not authorized.
    /// Every cached body is DROPPED and the builtins render: never serve a
    /// guessed or another tenant's text.
    Reset(u16),
    /// Network / decode / other non-2xx — LAST-GOOD kept.
    Kept(String),
}

/// Why a single document's BODY fetch produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BodyFetchError {
    /// 401 / 403 / 404 for this one document.
    Absent(u16),
    /// Network / decode / other non-2xx.
    Failed(String),
}

/// The edge-trigger key for a list outcome, for the same reason
/// [`plan_capture_log_key`] exists: a formatted reqwest error varies tick to
/// tick and would defeat "logged ONCE, on a transition".
fn briefing_list_log_key(outcome: &BriefingListOutcome) -> String {
    match outcome {
        BriefingListOutcome::Updated { versions, .. } => {
            let mut names: Vec<String> =
                versions.iter().map(|(n, v)| format!("{n}=v{v}")).collect();
            names.sort();
            format!("list:{}", names.join(","))
        }
        BriefingListOutcome::NotModified => "list:not_modified".to_string(),
        BriefingListOutcome::SkippedNoJwt => "list:skipped".to_string(),
        BriefingListOutcome::Reset(status) => format!("list:reset:{status}"),
        BriefingListOutcome::Kept(_) => "list:kept".to_string(),
    }
}

/// Pull the summary array out of coord's list response — `documents: [...]`
/// (the served shape), tolerating a bare top-level array.
fn briefing_versions_from(value: &serde_json::Value) -> std::collections::HashMap<String, i64> {
    let rows = value
        .get("documents")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .or_else(|| value.as_array().cloned())
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| {
            let name = row.get("name").and_then(serde_json::Value::as_str)?.trim();
            // Only the three names this runner renders. Extra rows an operator
            // creates under this kind are INERT by construction.
            if !BRIEFING_NAMES.contains(&name) {
                return None;
            }
            let version = row
                .get("current_version")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            Some((name.to_string(), version))
        })
        .collect()
}

/// Pull one document's body + version out of coord's row envelope
/// (`{document: {…}}` or flat) — the `continuation_verdict::rules_from_doc_body`
/// posture.
fn briefing_body_from(value: &serde_json::Value) -> Option<(String, i64)> {
    let doc = value.get("document").unwrap_or(value);
    let body = doc.get("body").and_then(serde_json::Value::as_str)?;
    if body.trim().is_empty() {
        return None;
    }
    let version = doc
        .get("current_version")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    Some((body.to_string(), version))
}

/// Does `status` mean "this document is absent, or not this device's to read"?
///
/// **403 and 404 only — deliberately NOT 401.** They are different claims:
///
/// - `403` / `404` are statements about the DOCUMENT. Dropping the cached body
///   is right: never keep serving text this tenant no longer has.
/// - `401` is a statement about the CREDENTIAL. Device JWTs live ~4h and
///   [`crate::coord_http::have_device_token`] only checks that a bearer
///   EXISTS, not that it is still valid — so an expired-but-present token
///   produces a 401 on a perfectly healthy tenant. Folding that into the reset
///   arm would drop every cached body, mark the cache changed, and rewrite the
///   on-disk store EMPTY: a token blip would permanently delete the tenant's
///   edited briefing. It is classified as a transient failure instead, which
///   keeps last-good.
fn is_absent_or_forbidden(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND
}

/// Is this the stale-credential status? Kept separate from
/// [`is_absent_or_forbidden`] so the distinction above is visible at both call
/// sites rather than implied by an omission.
fn is_stale_credential(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED
}

/// The ONE conditional LIST fetch per tick, through the mandated shared coord
/// seam ([`crate::coord_http::coord_client`] + [`crate::coord_http::coord_get`]).
///
/// A per-request timeout is set here because the shared client deliberately
/// carries no global one.
async fn fetch_briefing_list() -> BriefingListOutcome {
    if !crate::coord_http::have_device_token() {
        return BriefingListOutcome::SkippedNoJwt;
    }
    let Some(client) = crate::coord_http::coord_client() else {
        return BriefingListOutcome::Kept("shared coord client unavailable".to_string());
    };

    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    let mut req = crate::coord_http::coord_get(client, briefing_list_url(&base))
        .timeout(Duration::from_secs(10));
    if let Some(etag) = briefing_list_etag() {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return BriefingListOutcome::Kept(format!("request: {e}")),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return BriefingListOutcome::NotModified;
    }
    if is_stale_credential(status) {
        return BriefingListOutcome::Kept("coord rejected the device token (401)".to_string());
    }
    if is_absent_or_forbidden(status) {
        return BriefingListOutcome::Reset(status.as_u16());
    }
    if !status.is_success() {
        return BriefingListOutcome::Kept(format!("coord status {}", status.as_u16()));
    }

    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match resp.json::<serde_json::Value>().await {
        Ok(v) => BriefingListOutcome::Updated {
            versions: briefing_versions_from(&v),
            etag,
        },
        Err(e) => BriefingListOutcome::Kept(format!("decode: {e}")),
    }
}

/// Fetch ONE document's body. Unconditional by construction — coord's
/// single-document route emits no `ETag`, so there is nothing to validate
/// against, and the version gate above is what keeps this call rare.
async fn fetch_briefing_body(name: &str) -> Result<(String, i64), BodyFetchError> {
    let Some(client) = crate::coord_http::coord_client() else {
        return Err(BodyFetchError::Failed(
            "shared coord client unavailable".to_string(),
        ));
    };
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
    let resp = crate::coord_http::coord_get(client, briefing_url(&base, name))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| BodyFetchError::Failed(format!("request: {e}")))?;

    let status = resp.status();
    if is_stale_credential(status) {
        return Err(BodyFetchError::Failed(
            "coord rejected the device token (401)".to_string(),
        ));
    }
    if is_absent_or_forbidden(status) {
        return Err(BodyFetchError::Absent(status.as_u16()));
    }
    if !status.is_success() {
        return Err(BodyFetchError::Failed(format!(
            "coord status {}",
            status.as_u16()
        )));
    }
    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| BodyFetchError::Failed(format!("decode: {e}")))?;

    // A 2xx carrying no usable body is a MALFORMED answer, not a statement that
    // the document is absent — so it keeps last-good rather than resetting to
    // the builtin.
    briefing_body_from(&value)
        .ok_or_else(|| BodyFetchError::Failed("coord 2xx carried no document body".to_string()))
}

// ---- cache effects (each returns "the persisted content changed") ----------

/// Store a freshly fetched body. Always a content change worth persisting.
fn store_briefing(name: &str, body: String, version: i64) {
    if let Ok(mut g) = briefing_cache().write() {
        g.insert(
            name.to_string(),
            BriefingDocument {
                body,
                version,
                fetched_at: chrono::Utc::now().to_rfc3339(),
                provenance: BriefingProvenance::Coord,
            },
        );
    }
}

/// Drop one cached body — coord says the document is absent or this device may
/// not read it, so the compiled-in builtin renders.
fn drop_briefing(name: &str) -> bool {
    briefing_cache()
        .write()
        .map(|mut g| g.remove(name).is_some())
        .unwrap_or(false)
}

/// Mark one cached body as CONFIRMED against coord in this process: refresh its
/// stamp, and graduate a disk-restored entry from `cached` to `coord`. That
/// graduation is a persisted change; a plain re-confirmation is not.
fn confirm_briefing(name: &str) -> bool {
    let Ok(mut g) = briefing_cache().write() else {
        return false;
    };
    let Some(entry) = g.get_mut(name) else {
        return false;
    };
    entry.fetched_at = chrono::Utc::now().to_rfc3339();
    let promoted = entry.provenance == BriefingProvenance::Cached;
    entry.provenance = BriefingProvenance::Coord;
    promoted
}

/// A 304 on the list confirms every cached body at once.
fn confirm_all_briefings() -> bool {
    let mut changed = false;
    for name in BRIEFING_NAMES {
        changed |= confirm_briefing(name);
    }
    changed
}

/// The list validator to KEEP after a 200. PURE — this is the anti-wedge rule,
/// factored out of the loop so it is directly testable.
///
/// Storing the new etag while a body fetch is still outstanding wedges the
/// cache: the next tick would get a 304, which CONFIRMS what is cached rather
/// than retrying, and the document that failed would stay on the builtin until
/// somebody edited the list. So an unsettled tick keeps NO validator — the next
/// tick then pays one list body and retries. A 401/403/404 on a body is a
/// definitive answer and does not count as unsettled.
fn next_list_etag(every_body_settled: bool, etag: Option<&str>) -> Option<String> {
    if every_body_settled {
        etag.map(str::to_string)
    } else {
        None
    }
}

/// A 401/403/404 on the list drops every cached body at once.
fn clear_all_briefings() -> bool {
    let mut changed = false;
    for name in BRIEFING_NAMES {
        changed |= drop_briefing(name);
    }
    set_briefing_list_etag(None);
    changed
}

/// Poll the session-briefing documents once and apply the fail-safe contract,
/// logging each transition exactly once.
///
/// `last_logged` is keyed by document name (plus one `list:` key), because the
/// list and the three documents fail independently and a shared marker would
/// suppress one transition because another's had already been logged.
async fn poll_session_briefings_once(last_logged: &mut std::collections::HashMap<String, String>) {
    // Force the one-time disk restore HERE, off the spawn path, on every tick
    // including the ones that touch nothing else. Without this an unpaired or
    // offline runner never enters an arm that reads the cache, and the FIRST
    // `runner_context()` — which runs immediately before a PTY is opened — pays
    // the config-dir create plus the file read. The initializer is a `OnceLock`,
    // so every later call is free.
    let _ = cached_briefing(BRIEFING_RUNNER_SESSION);

    let list = fetch_briefing_list().await;
    let mut content_changed = false;

    match &list {
        // Unpaired, or a transient failure: the cache is left exactly as it
        // was, so the render keeps using the persisted last-good or the
        // builtin.
        BriefingListOutcome::SkippedNoJwt | BriefingListOutcome::Kept(_) => {}
        BriefingListOutcome::Reset(_) => {
            content_changed |= clear_all_briefings();
        }
        BriefingListOutcome::NotModified => {
            content_changed |= confirm_all_briefings();
        }
        BriefingListOutcome::Updated { versions, etag } => {
            // The new validator is stored only if EVERY body this list implied
            // was actually obtained. Storing it eagerly would wedge: a single
            // transient body-fetch failure would be followed by a 304 on the
            // next tick, which confirms the cache rather than retrying, and the
            // failed document would stay on the builtin until somebody edited
            // the list. Withholding the etag costs one extra list body per tick
            // until the retry succeeds, which is the right trade.
            let mut every_body_settled = true;
            for name in BRIEFING_NAMES {
                let Some(&listed) = versions.get(name) else {
                    // The row is not served to this device at all.
                    let dropped = drop_briefing(name);
                    content_changed |= dropped;
                    log_briefing_once(
                        last_logged,
                        name,
                        "absent",
                        format!(
                            "fleet_policy_poller: {BRIEFING_KIND}/{name} is not served to this \
                             device — rendering the compiled-in builtin"
                        ),
                        false,
                    );
                    continue;
                };

                // The version gate: a body is fetched ONLY when it moved.
                //
                // Version `0` is UNKNOWN on BOTH sides — it is what a list row
                // with no `current_version` decodes to and what a persisted
                // entry written by an older build carries — so `0 == 0` must
                // never read as "current". Letting it would promote a
                // disk-restored body from `cached` to `coord` and print
                // `[briefing: coord … v0]` for text this process never fetched,
                // which is exactly the claim the plan forbids.
                if listed > 0 && cached_briefing(name).is_some_and(|d| d.version == listed) {
                    content_changed |= confirm_briefing(name);
                    log_briefing_once(
                        last_logged,
                        name,
                        &format!("current:v{listed}"),
                        format!(
                            "fleet_policy_poller: {BRIEFING_KIND}/{name} is current at v{listed}"
                        ),
                        false,
                    );
                    continue;
                }

                match fetch_briefing_body(name).await {
                    Ok((body, body_version)) => {
                        // Prefer the body response's own version; fall back to
                        // the listed one when coord omitted it, so the version
                        // gate above cannot wedge on a perpetual 0.
                        let version = if body_version > 0 {
                            body_version
                        } else {
                            listed
                        };
                        store_briefing(name, body, version);
                        content_changed = true;
                        log_briefing_once(
                            last_logged,
                            name,
                            &format!("updated:v{version}"),
                            format!(
                                "fleet_policy_poller: {BRIEFING_KIND}/{name} updated to v{version} \
                                 (rendered on the next session spawn)"
                            ),
                            false,
                        );
                    }
                    Err(BodyFetchError::Absent(status)) => {
                        // A CONTRADICTION, not an answer: the list — same
                        // bearer, same tick — just served this row. Drop the
                        // body (fail-safe), but do NOT let the tick count as
                        // settled: keeping the validator would 304 the next
                        // tick, `confirm_all_briefings` would no-op on the
                        // absent name, and this document would sit on the
                        // builtin until somebody edited an unrelated row in the
                        // same kind.
                        every_body_settled = false;
                        content_changed |= drop_briefing(name);
                        log_briefing_once(
                            last_logged,
                            name,
                            &format!("reset:{status}"),
                            format!(
                                "fleet_policy_poller: coord returned {status} for the \
                                 {BRIEFING_KIND}/{name} body — dropping any cached body and \
                                 rendering the compiled-in builtin"
                            ),
                            false,
                        );
                    }
                    Err(BodyFetchError::Failed(e)) => {
                        every_body_settled = false;
                        log_briefing_once(
                            last_logged,
                            name,
                            "kept",
                            format!(
                                "fleet_policy_poller: {BRIEFING_KIND}/{name} body fetch failed \
                                 ({e}) — keeping the last-good body"
                            ),
                            true,
                        );
                    }
                }
            }
            set_briefing_list_etag(next_list_etag(every_body_settled, etag.as_deref()));
        }
    }

    // The list transition itself, on its own key.
    let list_key = briefing_list_log_key(&list);
    let (line, is_warn) = match &list {
        BriefingListOutcome::Updated { versions, .. } => (
            format!(
                "fleet_policy_poller: {BRIEFING_KIND} list refreshed ({} document(s) served)",
                versions.len()
            ),
            false,
        ),
        BriefingListOutcome::NotModified => (
            format!(
                "fleet_policy_poller: {BRIEFING_KIND} list unchanged (304) — cached bodies \
                 confirmed current"
            ),
            false,
        ),
        BriefingListOutcome::SkippedNoJwt => (
            format!(
                "fleet_policy_poller: no device JWT yet (unpaired) — skipping the \
                 {BRIEFING_KIND} list, briefings render from the persisted last-good or the \
                 compiled-in builtin"
            ),
            false,
        ),
        // 404 is the NORMAL answer until coord's half of the plan ships, so
        // this is info, not warn.
        BriefingListOutcome::Reset(status) => (
            format!(
                "fleet_policy_poller: coord returned {status} for the {BRIEFING_KIND} list \
                 (absent or unauthorized) — dropping any cached bodies and rendering the \
                 compiled-in builtins"
            ),
            false,
        ),
        BriefingListOutcome::Kept(e) => (
            format!(
                "fleet_policy_poller: {BRIEFING_KIND} list fetch failed ({e}) — keeping the \
                 last-good bodies"
            ),
            true,
        ),
    };
    log_briefing_once(last_logged, "list", &list_key, line, is_warn);

    if content_changed {
        persist_briefings();
    }
}

/// Emit `line` only when `key` differs from what was last logged for `slot`.
/// The edge-trigger behind "degradation is logged ONCE, on a transition".
fn log_briefing_once(
    last_logged: &mut std::collections::HashMap<String, String>,
    slot: &str,
    key: &str,
    line: String,
    is_warn: bool,
) {
    if last_logged.get(slot).map(String::as_str) == Some(key) {
        return;
    }
    last_logged.insert(slot.to_string(), key.to_string());
    if is_warn {
        warn!("{line}");
    } else {
        info!("{line}");
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
    /// Thread ceilings for the spawn gate's thread lane. **No coord column
    /// backs these yet** — they are named here so the term is plumbed end to
    /// end and starts working the day coord publishes it, which costs exactly
    /// the two `Option` fields below because this is a permissive response
    /// subset (see the type doc): today's payload carries neither key and both
    /// decode to `None`, which folds as "no fleet opinion".
    #[serde(default)]
    max_threads_sessions: Option<i64>,
    #[serde(default)]
    max_threads_sessions_critical: Option<i64>,
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

/// One control column → a thread CEILING this runner will honour. PURE.
///
/// The same three rules as [`floor_bytes`], read in the inverted direction:
/// absent ⇒ `None`, negative ⇒ `None` (an impossible value is UNKNOWN, and
/// `as u32` on a negative would produce a ~4-billion ceiling that never trips —
/// on a ceiling lane the sign error fails silently OPEN rather than closed, so
/// it is if anything easier to miss), and a value that does not fit a `u32` ⇒
/// `None` for the same reason.
///
/// Zero is preserved as `Some(0)` rather than folded into `None`, exactly as
/// [`floor_bytes`] preserves a zero floor: the fleet is entitled to say zero,
/// and saying it cannot make this machine unspawnable, because the effective
/// ceiling is a `min` that is clamped up at
/// [`crate::resource_guard::THREAD_CEILING_MIN`].
fn thread_ceiling(v: Option<i64>) -> Option<u32> {
    v.and_then(|n| u32::try_from(n).ok())
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
            ..SessionFloors::default()
        },
        wsl: SessionFloors {
            warn_free_bytes: floor_bytes(controls.min_free_bytes_sessions_wsl),
            critical_free_bytes: floor_bytes(controls.min_free_bytes_sessions_critical_wsl),
            ..SessionFloors::default()
        },
        threads: SessionFloors {
            warn_thread_count: thread_ceiling(controls.max_threads_sessions),
            critical_thread_count: thread_ceiling(controls.max_threads_sessions_critical),
            ..SessionFloors::default()
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
    fn count(v: Option<u32>) -> String {
        v.map_or_else(|| "unset".to_string(), |n| n.to_string())
    }
    format!(
        "host warn={} critical={}, wsl warn={} critical={}, threads warn={} critical={}",
        one(floors.host.warn_free_bytes),
        one(floors.host.critical_free_bytes),
        one(floors.wsl.warn_free_bytes),
        one(floors.wsl.critical_free_bytes),
        count(floors.threads.warn_thread_count),
        count(floors.threads.critical_thread_count),
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
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
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
    // One key PER DOCUMENT: the three session-briefing documents fail
    // independently (coord can serve one and 404 another), so a shared marker
    // would suppress one document's transition because another's was logged.
    let mut last_logged_briefings: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

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

        // Fourth cache: the three operator-editable session-briefing documents.
        // ONE conditional LIST GET per tick — coord validates `If-None-Match`
        // on the list route only — plus a body fetch for a document whose
        // version moved. Steady state is a single 304, which is the same "one
        // extra GET per tick beats a second loop" trade `CONTROLS_DOMAIN`
        // already makes. Each document applies to its own cache entry and logs
        // on its own transition, so no document's failure withholds another's.
        poll_session_briefings_once(&mut last_logged_briefings).await;

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
    let (base, _coord_base_source) = qontinui_runner_lib::profiles::coord_base_with_source();
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
        // …and the thread lane stays empty: this payload states no ceiling, and
        // an unstated ceiling is UNKNOWN, never a value.
        assert_eq!(floors.threads, SessionFloors::default());
    }

    /// The thread lane's wire fields, exercised against a payload **coord does
    /// not send today**.
    ///
    /// The spawn gate's thread ceiling (plan
    /// `2026-08-30-load-aware-spawn-admission-control`) is plumbed end to end
    /// while coord has no column to publish it from, so the live value is always
    /// `None` and the fold degrades to `min(local, hardcoded)`. This test is the
    /// only place that difference is observable: it proves the runner half is
    /// finished and will start honouring the term the day coord grows the
    /// column, rather than needing a second change then. `max_threads_sessions`
    /// is the name reserved for it.
    #[test]
    fn the_dormant_thread_ceilings_decode_when_coord_starts_sending_them() {
        let floors = floors_of(
            r#"{"domain":"fleet_resources","controls_available":true,
                "controls":{"max_threads_sessions":220,
                            "max_threads_sessions_critical":360}}"#,
        );
        assert_eq!(floors.threads.warn_thread_count, Some(220));
        assert_eq!(floors.threads.critical_thread_count, Some(360));
        // A count is not a byte floor: the thread lane carries no bytes and the
        // memory lanes carry no counts, whatever the payload says.
        assert_eq!(floors.threads.warn_free_bytes, None);
        assert_eq!(floors.host, SessionFloors::default());

        // A NEGATIVE count is impossible, so it is UNKNOWN — and the failure it
        // prevents is worse on a ceiling lane than on a floor one: `as u32` on
        // -1 is 4294967295, a ceiling nothing ever trips, so the sign error
        // would fail silently OPEN.
        let negative = floors_of(
            r#"{"domain":"fleet_resources","controls_available":true,
                "controls":{"max_threads_sessions":-1}}"#,
        );
        assert_eq!(negative.threads.warn_thread_count, None);
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
                ..SessionFloors::default()
            },
            wsl: SessionFloors {
                warn_free_bytes: Some(11),
                critical_free_bytes: None,
                ..SessionFloors::default()
            },
            threads: SessionFloors {
                warn_thread_count: Some(300),
                ..SessionFloors::default()
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
        // The thread lane is selected by the SAME key mechanism, and it carries
        // counts rather than bytes — a lane's reading is judged against its own
        // lane's limits or against nothing.
        assert_eq!(
            floors.for_lane(Lane::Threads.as_str()).warn_thread_count,
            Some(300)
        );
        assert_eq!(
            floors.for_lane(Lane::Host.as_str()).warn_thread_count,
            None,
            "a memory lane never carries a thread ceiling"
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
            "host warn=unset critical=unset, wsl warn=unset critical=unset, \
             threads warn=unset critical=unset"
        );

        let zeroed = SessionFloorsByLane {
            host: SessionFloors {
                warn_free_bytes: Some(0),
                critical_free_bytes: None,
                ..SessionFloors::default()
            },
            wsl: SessionFloors::default(),
            threads: SessionFloors::default(),
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

    // =======================================================================
    // Cache #4 — the session-briefing documents (plan
    // 2026-08-20-runner-session-briefing-versioned-and-operator-editable)
    // =======================================================================

    /// The `(kind, name)` contract with coord. Constants on both sides, so a
    /// rename has to be a two-repo change rather than a silent 404.
    #[test]
    fn the_briefing_document_contract_is_three_fixed_names() {
        assert_eq!(BRIEFING_KIND, "session_briefing");
        assert_eq!(BRIEFING_RUNNER_SESSION, "runner-session");
        assert_eq!(BRIEFING_PLAN_CAPTURE_CLAUSE, "plan-capture-clause");
        assert_eq!(BRIEFING_AI_SESSION_RULES, "ai-session-rules");
        assert_eq!(BRIEFING_NAMES.len(), 3);
        for name in BRIEFING_NAMES {
            assert!(!name.is_empty());
        }
    }

    /// Both URL builders name the AGENT door. The operator sibling
    /// `/coord/prompt-documents/*` 403s a device JWT, so building it would make
    /// the fetch fail exactly when it is needed.
    #[test]
    fn the_briefing_urls_name_the_agent_door() {
        let list = briefing_list_url("https://coord.example.com/");
        assert_eq!(
            list,
            "https://coord.example.com/coord/agent-prompt-documents?kind=session_briefing"
        );
        let one = briefing_url("https://coord.example.com", BRIEFING_RUNNER_SESSION);
        assert_eq!(
            one,
            "https://coord.example.com/coord/agent-prompt-documents/session_briefing/runner-session"
        );
        // The full-segment negative: `agent-prompt-documents` CONTAINS
        // `prompt-documents`, so match the whole path.
        for url in [&list, &one] {
            assert!(!url.contains("/coord/prompt-documents"), "{url}");
        }
    }

    /// The list decode keeps only the three names this runner renders. Extra
    /// rows an operator creates under this kind are INERT by construction — the
    /// web create dialog will happily let them make one.
    #[test]
    fn the_list_decode_keeps_only_the_three_known_names() {
        let body = serde_json::json!({"documents": [
            {"name": "runner-session", "current_version": 7},
            {"name": "plan-capture-clause", "current_version": 3},
            {"name": "somebodys-fourth-row", "current_version": 1},
        ]});
        let versions = briefing_versions_from(&body);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions.get("runner-session"), Some(&7));
        assert_eq!(versions.get("plan-capture-clause"), Some(&3));
        assert_eq!(versions.get("somebodys-fourth-row"), None);
    }

    /// …and tolerates a bare top-level array, the shape `prompt_library` also
    /// accepts.
    #[test]
    fn the_list_decode_tolerates_a_bare_array() {
        let body = serde_json::json!([{"name": "ai-session-rules", "current_version": 2}]);
        assert_eq!(
            briefing_versions_from(&body).get("ai-session-rules"),
            Some(&2)
        );
    }

    /// The single-document decode reads both envelope shapes, and an EMPTY body
    /// is not a document. An empty prompt would otherwise silently replace a
    /// working briefing with nothing.
    #[test]
    fn the_body_decode_reads_both_envelopes_and_refuses_an_empty_body() {
        let flat = serde_json::json!({"body": "hello", "current_version": 9});
        assert_eq!(briefing_body_from(&flat), Some(("hello".to_string(), 9)));

        let enveloped = serde_json::json!({"document": {"body": "hi", "current_version": 4}});
        assert_eq!(briefing_body_from(&enveloped), Some(("hi".to_string(), 4)));

        assert_eq!(
            briefing_body_from(&serde_json::json!({"body": "  \n "})),
            None
        );
        assert_eq!(briefing_body_from(&serde_json::json!({})), None);
    }

    /// The edge-trigger key collapses a REPEATED failure to one line — the
    /// formatted error varies tick to tick (a DNS detail, an ephemeral port),
    /// which would otherwise turn "logged once" into a line per 45s under
    /// exactly the sustained-failure conditions the rule exists for.
    #[test]
    fn a_repeated_list_failure_collapses_to_one_log_key() {
        let a = BriefingListOutcome::Kept("dns error 11001".to_string());
        let b = BriefingListOutcome::Kept("os error 10060".to_string());
        assert_eq!(briefing_list_log_key(&a), briefing_list_log_key(&b));
        // …while a changed STATUS still opens a new one.
        assert_ne!(
            briefing_list_log_key(&a),
            briefing_list_log_key(&BriefingListOutcome::Reset(404))
        );
        assert_ne!(
            briefing_list_log_key(&BriefingListOutcome::Reset(401)),
            briefing_list_log_key(&BriefingListOutcome::Reset(404))
        );
    }

    /// A changed VERSION opens a new log line — that is the transition an
    /// operator needs to see.
    #[test]
    fn a_changed_version_is_a_new_list_log_key() {
        let one = BriefingListOutcome::Updated {
            versions: [("runner-session".to_string(), 7)].into_iter().collect(),
            etag: None,
        };
        let two = BriefingListOutcome::Updated {
            versions: [("runner-session".to_string(), 8)].into_iter().collect(),
            etag: None,
        };
        assert_ne!(briefing_list_log_key(&one), briefing_list_log_key(&two));
    }

    /// `log_briefing_once` is the edge trigger itself: same key ⇒ silent, new
    /// key ⇒ emitted, and the slots are independent so one document's steady
    /// state cannot suppress another's transition.
    #[test]
    fn the_log_edge_trigger_is_per_slot() {
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        log_briefing_once(&mut seen, "list", "kept", "x".to_string(), false);
        assert_eq!(seen.get("list").map(String::as_str), Some("kept"));
        log_briefing_once(
            &mut seen,
            "runner-session",
            "updated:v1",
            "y".to_string(),
            false,
        );
        assert_eq!(seen.len(), 2);
        // Same key on an existing slot leaves it untouched.
        log_briefing_once(&mut seen, "list", "kept", "z".to_string(), false);
        assert_eq!(seen.get("list").map(String::as_str), Some("kept"));
    }

    /// The cache's fail-safe RESTING value is EMPTY, i.e. every render falls
    /// back to the compiled-in builtin. This is the arm that runs on every
    /// runner until coord's half of the plan ships.
    #[test]
    fn the_briefing_cache_rests_empty() {
        let _pin = pin_plan_capture_level_for_test("off");
        for name in BRIEFING_NAMES {
            assert_eq!(cached_briefing(name), None);
        }
    }

    /// STORE ⇒ readable and labelled `coord`. DROP ⇒ gone, so the builtin
    /// renders. Both drive the SHIPPING cache-effect functions, not a copy.
    #[test]
    fn store_then_drop_round_trips_the_cache() {
        let _pin = pin_plan_capture_level_for_test("off");

        store_briefing(BRIEFING_RUNNER_SESSION, "body".to_string(), 7);
        let doc = cached_briefing(BRIEFING_RUNNER_SESSION).expect("stored");
        assert_eq!(doc.body, "body");
        assert_eq!(doc.version, 7);
        assert_eq!(doc.provenance, BriefingProvenance::Coord);
        assert!(!doc.fetched_at.is_empty());

        assert!(drop_briefing(BRIEFING_RUNNER_SESSION));
        assert_eq!(cached_briefing(BRIEFING_RUNNER_SESSION), None);
        // Dropping an absent document is not a content change.
        assert!(!drop_briefing(BRIEFING_RUNNER_SESSION));
    }

    /// A 304 on the LIST confirms every cached body: a disk-restored entry
    /// GRADUATES from `cached` to `coord` (a persisted change), while an
    /// already-confirmed one is merely re-stamped (not a persisted change, so
    /// the store is not rewritten every 45s forever).
    #[test]
    fn confirming_promotes_a_disk_restored_body_exactly_once() {
        let pin = pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            briefing_for_test("restored", 4, BriefingProvenance::Cached),
        );

        assert!(
            confirm_all_briefings(),
            "the graduation is a content change"
        );
        assert_eq!(
            cached_briefing(BRIEFING_RUNNER_SESSION).unwrap().provenance,
            BriefingProvenance::Coord
        );
        assert!(
            !confirm_all_briefings(),
            "a re-confirmation must not rewrite the store"
        );
    }

    /// A 401/403/404 on the LIST drops every cached body AND the list
    /// validator, so the next 2xx re-fetches rather than replaying a stale
    /// `If-None-Match` against a set the runner no longer holds.
    #[test]
    fn a_list_reset_clears_every_body_and_the_validator() {
        let pin = pin_plan_capture_level_for_test("off");
        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            briefing_for_test("a", 1, BriefingProvenance::Coord),
        );
        pin.set_briefing(
            BRIEFING_AI_SESSION_RULES,
            briefing_for_test("b", 2, BriefingProvenance::Coord),
        );
        set_briefing_list_etag(Some("\"abc\"".to_string()));

        assert!(clear_all_briefings());
        for name in BRIEFING_NAMES {
            assert_eq!(cached_briefing(name), None);
        }
        assert_eq!(briefing_list_etag(), None);
    }

    /// The anti-wedge rule. A tick that could not settle every body keeps NO
    /// validator, so the next tick gets a 200 it can retry from rather than a
    /// 304 that would confirm a cache still missing a document.
    #[test]
    fn an_unsettled_tick_keeps_no_list_validator() {
        assert_eq!(
            next_list_etag(true, Some("\"abc\"")),
            Some("\"abc\"".to_string())
        );
        assert_eq!(next_list_etag(false, Some("\"abc\"")), None);
        // A coord that serves no ETag at all simply never gets a conditional
        // request — that is a missing optimisation, not a wedge.
        assert_eq!(next_list_etag(true, None), None);
    }

    /// 401 is a CREDENTIAL statement, 403/404 are DOCUMENT statements. Folding
    /// 401 into the reset arm would let an expired device JWT — which the
    /// pre-fetch token check cannot detect, since it only proves a bearer
    /// EXISTS — drop every cached body and rewrite the on-disk store empty.
    #[test]
    fn a_401_is_a_stale_credential_not_an_absent_document() {
        let code = |c: u16| reqwest::StatusCode::from_u16(c).unwrap();
        assert!(is_stale_credential(code(401)));
        assert!(!is_absent_or_forbidden(code(401)));
        for c in [403u16, 404] {
            assert!(is_absent_or_forbidden(code(c)));
            assert!(!is_stale_credential(code(c)));
        }
        for c in [200u16, 304, 429, 500, 503] {
            assert!(!is_absent_or_forbidden(code(c)));
            assert!(!is_stale_credential(code(c)));
        }
    }

    /// The restore rules: unknown names dropped, and a file CLAIMING `coord`
    /// force-relabelled to `cached`. A hand-edited store must not be able to
    /// buy a body a provenance label it has not earned in this process.
    #[test]
    fn the_restore_drops_unknown_names_and_never_trusts_a_claimed_provenance() {
        let raw = r#"{
            "runner-session": {"body": "a", "version": 7, "provenance": "coord"},
            "somebodys-fourth-row": {"body": "b", "version": 1}
        }"#;
        let restored = restore_briefings_from_str(raw);
        assert_eq!(restored.len(), 1, "unknown names must be dropped");
        let doc = restored.get("runner-session").expect("present");
        assert_eq!(doc.version, 7);
        assert_eq!(
            doc.provenance,
            BriefingProvenance::Cached,
            "a claimed `coord` must be force-relabelled"
        );
    }

    /// The briefing rows of [`dial_snapshot`], which nothing exercised.
    ///
    /// `config_report_cmd` already renders a `None` version as `v?` and an
    /// empty stamp as `UNKNOWN`. The stamp arm was reachable; the version arm
    /// was not, because a PRESENT document mapped to `Some(doc.version)`
    /// unconditionally — so a document the runner holds at the UNKNOWN version
    /// `0` was reported to the operator as `runner-session=v0`, a generation
    /// number it does not have. Same rule as the version gate in this module
    /// and as `session_briefing::Provenance`.
    #[test]
    fn the_briefing_dial_reports_an_unknown_version_as_absent() {
        let pin = pin_plan_capture_level_for_test("off");

        pin.set_briefing(
            BRIEFING_RUNNER_SESSION,
            briefing_for_test("body", 7, BriefingProvenance::Coord),
        );
        let mut zero = briefing_for_test("body", 0, BriefingProvenance::Cached);
        zero.fetched_at = String::new();
        pin.set_briefing(BRIEFING_PLAN_CAPTURE_CLAUSE, zero);

        let dial = dial_snapshot();
        let row = |name: &str| {
            dial.briefings
                .iter()
                .find(|b| b.name == name)
                .expect("every BRIEFING_NAMES entry has a row")
        };

        let known = row(BRIEFING_RUNNER_SESSION);
        assert!(known.present);
        assert_eq!(known.version, Some(7));
        assert_eq!(known.provenance, Some("coord"));
        assert_eq!(
            known.fetched_at.as_deref(),
            Some("2026-08-20T00:00:00+00:00")
        );

        // PRESENT with an UNKNOWN version — the two are independent facts, and
        // reporting the document as absent would be the opposite lie.
        let unknown = row(BRIEFING_PLAN_CAPTURE_CLAUSE);
        assert!(unknown.present);
        assert_eq!(unknown.version, None);
        assert_eq!(unknown.fetched_at, None);
        assert_eq!(unknown.provenance, Some("cached"));

        // A name with nothing cached still gets a row, marked absent.
        let missing = row(BRIEFING_AI_SESSION_RULES);
        assert!(!missing.present);
        assert_eq!(missing.version, None);
        assert_eq!(missing.provenance, None);
    }

    /// **The briefing store's READ path creates nothing** — driven against a
    /// root the test owns, so the real [`load_persisted_briefings_from`] body
    /// runs rather than the `#[cfg(test)]` twin of [`initial_briefing_cache`].
    ///
    /// This is the half that used to be untestable. `briefing_store_path` went
    /// through `settings`' ensuring resolver, and every door into
    /// [`briefing_cache`] is a READ: `config_report`'s layer 11 reaches it via
    /// `dial_snapshot` → `briefing_snapshot`, and its env-generation section
    /// reaches it a second way via `pty_child_command` →
    /// `terminal::runner_context` → [`cached_briefing`]. Whichever gets there
    /// first initialises the `OnceLock`, both before the report stats the
    /// directory — so a typo'd `QONTINUI_CONFIG_DIR` was CREATED by the
    /// diagnostic and then reported as present.
    #[test]
    fn the_briefing_store_read_path_creates_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // (1) An absent store under an absent parent is "first boot", and
        // neither is brought into existence by asking for it.
        let typo = tmp.path().join("qonitnui-typo");
        let absent = typo.join(BRIEFING_STORE_FILE);
        assert!(!typo.exists(), "fixture precondition");
        assert!(
            load_persisted_briefings_from(&absent).is_empty(),
            "an absent store is first boot"
        );
        assert!(
            !typo.exists(),
            "reading the store must not materialize the config directory"
        );
        assert!(!absent.exists(), "nor the store file");

        // (2) A REAL store under a root the test owns restores — so (1) is a
        // statement about non-creation, not about the reader being inert.
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).expect("mkdir");
        let store = real.join(BRIEFING_STORE_FILE);
        std::fs::write(
            &store,
            br#"{"runner-session":{"body":"restored","version":3,"provenance":"coord"}}"#,
        )
        .expect("write store");
        let restored = load_persisted_briefings_from(&store);
        let doc = restored.get("runner-session").expect("restored");
        assert_eq!(doc.body, "restored");
        assert_eq!(doc.version, 3);
        assert_eq!(doc.provenance, BriefingProvenance::Cached);
    }

    /// …and the path RESOLUTION itself goes through the non-creating resolver —
    /// asserted BOTH on the source and on the behaviour, because neither half
    /// is sufficient alone.
    ///
    /// The behavioural test above owns its path, so it cannot see which
    /// resolver [`briefing_store_path`] picks — and that choice IS the defect:
    /// the two resolvers return the same `PathBuf` and differ only in a side
    /// effect, which no equality assertion over the RETURN VALUE can
    /// distinguish. So part (1) asserts on the source of the resolution, the way
    /// `agent_worktree::census` and `ci_node::services` already assert source
    /// invariants in this crate. Restore the ensuring call and it fails.
    ///
    /// But a source invariant bans a SPELLING, not a behaviour, and this body
    /// reintroduces the whole defect while satisfying every needle in part (1):
    ///
    /// ```ignore
    /// let (dir, _) = crate::settings::resolve_config_dir().ok()?;
    /// std::fs::create_dir_all(&dir).ok();
    /// Some(dir.join(BRIEFING_STORE_FILE))
    /// ```
    ///
    /// So part (2) drives the real function against a config dir the test owns
    /// and asserts the directory is still absent afterwards — the property
    /// itself, immune to how the creation is spelled. `create_dir_all` is added
    /// to the banned needles as well, but as belt-and-braces behind the
    /// behavioural check, not in place of it.
    ///
    /// Part (2) holds `env_lock` for its whole body, which is what makes the
    /// `set_var("QONTINUI_CONFIG_DIR", …)` safe: the flake class this test's
    /// previous round avoided (cf.
    /// `agent_runtime::…::coord_ws_url_resolves_on_hosted_tier_with_no_profile_coord_url`)
    /// comes from reading or writing a process-global variable WITHOUT the lock,
    /// not from touching it at all. `EnvVarRestore` puts the variable back on
    /// the panic path too.
    #[test]
    fn briefing_store_path_resolves_the_config_dir_without_creating_it() {
        // ---- (1) the SOURCE invariant: which resolver the body names -------
        const SRC: &str = include_str!("fleet_policy_poller.rs");
        let body: Vec<&str> = SRC
            .lines()
            .skip_while(|l| !l.contains("fn briefing_store_path()"))
            .skip(1)
            .take_while(|l| !l.starts_with('}'))
            .collect();
        assert!(
            !body.is_empty(),
            "briefing_store_path not found — re-derive this test against the renamed function"
        );
        let body = body.join("\n");
        assert!(
            body.contains("settings::resolve_config_dir()"),
            "the store path must resolve through the NON-creating resolver, got:\n{body}"
        );
        for banned in [
            // Split so these needles are not the thing they find.
            concat!("settings::get_", "config_dir"),
            concat!("create_dir", "_all"),
        ] {
            assert!(
                !body.contains(banned),
                "{banned} creates the config directory, and every caller of this function is a \
                 read — see its docs. Got:\n{body}"
            );
        }

        // ---- (2) the BEHAVIOURAL invariant: nothing is created -------------
        // Held for the whole of part (2): `QONTINUI_CONFIG_DIR` is
        // process-global, and this is the only safe way to point the REAL
        // resolver at a directory the test owns.
        let _env = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&["QONTINUI_CONFIG_DIR"]);

        let tmp = tempfile::tempdir().expect("tempdir");
        let typo = tmp.path().join("qonitnui-typo");
        assert!(!typo.exists(), "fixture precondition");
        std::env::set_var("QONTINUI_CONFIG_DIR", &typo);

        // The join is the on-disk contract, asserted as a LITERAL — and now as
        // a WHOLE path, because the lock above makes the directory half stable
        // for the duration of this body.
        assert_eq!(BRIEFING_STORE_FILE, "session-briefing.cache.json");
        let path = briefing_store_path().expect("an env override always resolves");
        assert_eq!(path, typo.join("session-briefing.cache.json"));
        assert!(
            !typo.exists(),
            "asking WHERE the briefing store lives must not create the config directory — that \
             is the fault `config_report` opens to explain, and creating it destroys the evidence"
        );
        assert!(!path.exists(), "nor the store file itself");
    }

    /// A corrupt store is logged and ignored — never a panic, never a partial.
    #[test]
    fn a_corrupt_store_restores_empty_rather_than_panicking() {
        assert!(restore_briefings_from_str("{ not json").is_empty());
        assert!(restore_briefings_from_str("").is_empty());
    }

    /// The persisted store round-trips through serde with every field
    /// defaulted, so a file written by another build restores rather than
    /// failing the whole load.
    #[test]
    fn a_partial_persisted_document_still_decodes() {
        let raw = r#"{"runner-session":{"body":"hi"}}"#;
        let loaded: std::collections::HashMap<String, BriefingDocument> =
            serde_json::from_str(raw).expect("a partial store must decode");
        let doc = loaded.get("runner-session").expect("present");
        assert_eq!(doc.body, "hi");
        assert_eq!(doc.version, 0);
        assert_eq!(doc.fetched_at, "");
        // A restored document is `cached` by default — it has NOT been checked
        // against coord in this process.
        assert_eq!(doc.provenance, BriefingProvenance::Cached);
    }
}
