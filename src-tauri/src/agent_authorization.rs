//! Agent-registry spawn authorization — the runner-side enforcement layer for
//! the served policy clause `agent-spawn-authorization`
//! (`policy/production-and-cost`, operator-decided 2026-07-27).
//!
//! # What this is
//!
//! Every runner surface that spawns an agent — an in-session helper, a
//! `/sessions/spawn` continuation, a gate continuation, a looping-agent
//! relaunch — funnels through [`authorize_spawn`] before it spawns. The
//! decision comes from the tenant's agent registry, which coord resolves for
//! this device (registry defaults ∩ the user's recorded per-agent prefs) and
//! serves at:
//!
//! ```text
//! GET {coord}/coord/agent-registry/effective
//! {"agents":[{agent_name, purpose, spawn_path, model, effort,
//!             policy_required, fanout_bound, enabled, disposition, source}]}
//! ```
//!
//! Tenant and user are resolved server-side from the device JWT, so the cache
//! below is keyed by nothing but this process's device identity.
//!
//! # What this is NOT
//!
//! Not metering, not cost estimation, not attribution. The clause is explicit:
//! "Qontinui is NOT a token middleman: the user owns the subscription/API key
//! and manages the spend, so this clause governs WHETHER a spawn happens,
//! never what it costs. Do not build metering, estimation, disclosure, or
//! attribution."
//!
//! # The fail-safe matrix
//!
//! The failure directions are the whole point, so they are stated exhaustively
//! (and are covered 1:1 by the tests at the bottom of this file):
//!
//! | registry state | agent row | class | decision |
//! |---|---|---|---|
//! | resolved (fresh or stale) | present + class-matching, `enabled=true` | any | `Allow` |
//! | resolved | present, `enabled=false`, `disposition=block` | any | `Deny` |
//! | resolved | present, `enabled=false`, `disposition=degrade` | any | `DegradeToInline` |
//! | resolved | present, `enabled=false`, `disposition=warn_proceed` | any | `Warn` |
//! | resolved | present, `enabled=false`, **no** disposition | any | `DegradeToInline` (documented fallback) |
//! | resolved | absent | `standing_continuation` | `Deny` (default-OFF; needs an explicit opt-in row) |
//! | resolved | absent | other | `Allow` (legacy default, logged) |
//! | unresolvable, snapshot held and younger than [`MAX_SNAPSHOT_AGE`] | — | — | decided from the stale snapshot, flagged with its age |
//! | unresolvable, no usable snapshot | — | `standing_continuation` | `Deny` — a standing opt-in is never presumed |
//! | unresolvable, no usable snapshot | `policy_required` (cold-start floor) | non-standing | `DegradeToInline` — never silently skip a required gate |
//! | unresolvable, no usable snapshot | optional | non-standing | `Allow` — a coord outage must not break the fleet |
//! | runner unpaired (no device JWT), no snapshot | — | any | `Allow` — there is no tenant, so there is no preference to honour |
//! | coord answers **404** | — | any | `Allow` (registry endpoint not deployed on this coord; logged at `error!`) |
//!
//! Note the class column on the `policy_required` row: the standing check runs
//! FIRST, so a policy-required *standing* spawn is denied rather than degraded.
//! Denying is the stronger of the two and the standing opt-in is the harder
//! rule ("never implied by a task"), so it wins.
//!
//! Three rows deserve their reasoning spelled out:
//!
//! * **Stale beats cold, but not forever.** The last-known-good snapshot
//!   outlives its freshness — that is what keeps a coord outage from tripping
//!   the `standing_continuation` default-OFF rule on a fleet that has already
//!   resolved the registry once. But it is capped at [`MAX_SNAPSHOT_AGE`]:
//!   past that, a revoked credential or a permanently unreachable coord would
//!   otherwise let a day-old snapshot override a preference the user has since
//!   changed. Beyond the cap the cold matrix applies.
//! * **Unpaired is not an outage.** A runner with no device JWT has no coord
//!   tenant, so there is no recorded user preference to honour — denying would
//!   *invent* one and would stop local looping agents on an offline runner.
//!   Same category as "coord has no registry", not "coord is down".
//! * **404 is a rollout state, not an outage.** This module ships ahead of the
//!   coord endpoint (its PR is DRAFT-held on that dependency). A coord build
//!   without the route must not hard-deny every standing spawn on the fleet, so
//!   a 404 specifically reads as "registry feature absent" → allow, loudly.
//!   **Fence:** [`fetch_effective`] refuses to issue the request at all without
//!   a device JWT, so this arm is only ever reached by an *authenticated* 404 —
//!   a proxy that 404s unauthenticated probes to hide routes cannot trigger it.
//!   **Sunset:** this arm is temporary and its removal is tracked by coord gate
//!   `706b53d5-c68e-4b80-baf2-31e71a385cb9` (work unit
//!   `2026-07-28-migrate-claude-md-into-qontinui`, phase "Phase 4c — sunset the
//!   runner 404 allow-all rollout arm"). Delete [`Fetched::NotDeployed`], the
//!   [`Resolution::NotDeployed`] arm, the `registry-endpoint-not-deployed` rule
//!   and its tests once the endpoint returns 200 in production, so an
//!   unexpected 404 falls through to the normal unresolvable matrix. A rollout
//!   allow-everything with no removal trigger is how a gate quietly dies.
//!   Every OTHER failure (timeout, connection refused, 401/403, 5xx, decode
//!   error) is a genuine unresolvable and takes the matrix above.
//!
//! # Spawn-path classes
//!
//! [`SpawnPath`] mirrors the registry's `spawn_path` column. The class matters
//! because the clause treats them differently: an in-session subagent is
//! *implied-by-task, bounded*, whereas anything that OUTLIVES the request
//! ("continuation sessions, shepherd spawns, auto-dispatch") is a **standing
//! per-path opt-in, default OFF for a fresh user; never implied by a task."
//! That default-OFF is the generalization of `agent_desired_state`'s
//! `armed=true` contract. The operator's own tenant is seeded opted-IN by the
//! web seed script, so fleet autonomy does not regress.
//!
//! A row is only consulted for the class it declares: an agent the user enabled
//! for `in_session_subagent` use does NOT thereby opt into standing spawns of
//! the same agent (see [`lookup_row`]). Otherwise an ordinary in-session
//! opt-in would silently imply the standing one.
//!
//! # Parallel fan-out bound
//!
//! The clause's other half: *"Parallel fan-out: bound declared up front
//! (registry default 15). On breach, degrade to sequential and keep working —
//! never hard-fail mid-task. Escalate only if sequential still cannot
//! complete."*
//!
//! [`authorize_fanout_spawn`] is the entry point for the `parallel_fanout`
//! class. It runs the same matrix as [`authorize_spawn`] and then, when the
//! spawn is authorized, admits it against the governing row's `fanout_bound`
//! ([`DEFAULT_FANOUT_BOUND`] when no row declares one, clamped to at least 1):
//!
//! * a free slot → [`SpawnDecision::Allow`] plus a [`FanoutSlot`];
//! * no free slot → the caller **queues** for one and, once admitted, gets
//!   [`SpawnDecision::SerializeToBound`] plus the slot. This is the degrade to
//!   sequential: the work still happens, one at a time. It is deliberately NOT
//!   a refusal — [`SpawnDecision::allows_spawn`] is true for it and
//!   [`SpawnDecision::refusal`] is `None`, so a breach can never be mistaken
//!   for a denial by a call site that does not know the variant.
//! * nothing frees within [`FANOUT_WAIT_BUDGET`] → [`FanoutAdmission::SlotUnavailable`],
//!   which is **transient**: the caller retries later rather than failing the
//!   task. That is the only outcome that can stall work, and it is the point
//!   the clause says to escalate from ("the declared fan-out bound is reached
//!   with work outstanding and sequential degradation cannot complete").
//!
//! Accounting is a [`tokio::sync::Semaphore`] per lane, keyed by the row that
//! SUPPLIED the bound ([`Verdict::bound_source`]) so that several agents falling
//! back to one class row share that row's bound instead of getting one bound
//! each. The decrement is the permit's `Drop`, so it runs on the happy path, on
//! every `?`/early return, and on unwind from a panic — a leaked count would
//! wedge fan-out for the life of the process, which is strictly worse than the
//! unenforced bound this replaced.
//!
//! ## Two layers, because one is not enough
//!
//! A [`FanoutSlot`] covers a spawn's *admission* — the critical section the
//! caller holds it across — not the spawned worker's whole life. That alone
//! would be nominal enforcement: the orchestration conductor dispatches
//! sequentially within a tick, so its admission occupancy is ≤ 1 and a bound of
//! 15 could never bind. Bounding a worker's lifetime with the same guard would
//! need a release hook on a path a crash or a missed reconcile can skip, i.e.
//! exactly the leak this shape rules out.
//!
//! So the bound is enforced twice, at the two points that can actually observe
//! it:
//!
//! 1. **Batch sizing (the layer that binds).** [`current_fanout_bound`] is a
//!    read-only resolve the orchestration conductor folds into its per-tick
//!    concurrency cap (`min(concurrency_cap, fanout_bound)`), so the number of
//!    workers *alive at once* honours the registry. It is leak-proof by
//!    construction: the conductor re-derives in-flight count from the durable
//!    rows every tick and carries nothing in memory. A cap that admits fewer
//!    dispatches than there is ready work IS the clause's degrade to
//!    sequential — the remaining subtasks stay queued and land on later ticks.
//! 2. **Admission (the layer that is exact).** [`authorize_fanout_spawn`]
//!    additionally serializes concurrent *admissions* across callers that do
//!    not share a conductor (several runs, several funnel sites), which the
//!    per-tick cap cannot see.
//!
//! # Known gaps (stated, not implied)
//!
//! * The cold-start `policy_required` floor is currently unreachable from every
//!   wired call site, because no runner funnel site names a subagent that is
//!   both policy-required and non-standing. It is kept because the matrix cell
//!   is specified and becomes live the moment such a site exists.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde::Deserialize;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tracing::{error, info, warn};

/// How long a successfully-resolved registry snapshot counts as fresh.
///
/// 90s sits in the middle of the plan's "cached with TTL" band: long enough
/// that a burst of spawns costs one coord round-trip, short enough that a user
/// toggling an agent off in the web settings page sees it take effect on the
/// fleet within about a minute (the same order as the tenant-policy poll).
pub const REGISTRY_CACHE_TTL: Duration = Duration::from_secs(90);

/// Hard ceiling on how old a last-known-good snapshot may be and still decide.
///
/// Past this, the snapshot is dropped and the cold matrix applies. Without a
/// cap, a permanently-failing resolve (revoked device JWT, coord decommissioned)
/// would honour a snapshot from days ago forever — silently ignoring a
/// preference the user changed in the meantime.
const MAX_SNAPSHOT_AGE: Duration = Duration::from_secs(12 * 60 * 60);

/// After a failed resolve, how long before another spawn re-attempts the HTTP
/// call. Without this, every spawn during a coord outage would pay the full
/// [`HTTP_TIMEOUT`]; with it, the last-known-good snapshot answers immediately
/// and only one attempt per window touches the network.
const REGISTRY_ERROR_BACKOFF: Duration = Duration::from_secs(15);

/// Per-attempt timeout for the registry GET.
///
/// Deliberately short. This read sits in front of interactive spawns (the
/// runner UI's "New Chat") and, historically, in front of the coord WS pump —
/// a gate check has no business costing seconds. One attempt per
/// [`REGISTRY_ERROR_BACKOFF`] window pays it; every other caller is served
/// from cache.
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

const REGISTRY_PATH: &str = "/coord/agent-registry/effective";

/// Cold-start floor for `policy_required`.
///
/// `policy_required` normally comes from the registry row — but the one case
/// the matrix must still get right is "coord has NEVER been reachable in this
/// process AND the caller names a policy-required agent", where there is no row
/// to read it from. Skipping the gate silently is the outcome the clause
/// forbids, so a named agent on this list degrades to inline instead. Keep it
/// minimal — it exists only to cover the cold-start window, not to duplicate
/// the registry. Both spellings are listed because the registry key is coord's
/// to choose and a rename must not silently disarm the floor. Override with
/// `QONTINUI_SPAWN_AUTHZ_POLICY_REQUIRED_FLOOR` (comma-separated agent names;
/// an empty value disables the floor).
const DEFAULT_POLICY_REQUIRED_FLOOR: &[&str] = &["code-reviewer", "code_reviewer"];

/// Operator break-glass, mirroring `QONTINUI_COORD_GUARD=skip`. Set
/// `QONTINUI_SPAWN_AUTHZ_DISABLED=1` to bypass the gate's FAIL-SAFE refusals.
///
/// It deliberately does NOT override an explicit user disable — see
/// [`break_glass_may_override`]. Every bypassed decision is logged at `warn!`,
/// and the flag is announced once at startup by [`log_break_glass_at_startup`];
/// there is no quiet bypass.
fn authz_disabled() -> bool {
    matches!(
        std::env::var("QONTINUI_SPAWN_AUTHZ_DISABLED")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Whether the operator break-glass may override a verdict produced by `rule`.
///
/// The clause admits no operator override of a recorded user choice — "A spawn
/// the user has explicitly deselected in the agent registry: never — no policy,
/// project file, or launcher instruction may re-mandate it." So the break-glass
/// covers only the gate's own FAIL-SAFE refusals (unresolvable, default-OFF,
/// missing row), never a `registry-row-disabled-*` verdict.
fn break_glass_may_override(rule: &str) -> bool {
    !rule.starts_with("registry-row-disabled")
}

/// Announce the break-glass once, loudly, at runner startup.
///
/// A per-spawn `warn!` in a busy log is not discoverable; an operator who set
/// this env var months ago has no other signal that the gate is off. Called
/// from the runner's startup path.
pub fn log_break_glass_at_startup() {
    if authz_disabled() {
        error!(
            "QONTINUI_SPAWN_AUTHZ_DISABLED is set — agent-registry spawn authorization \
             is BYPASSED for every fail-safe refusal on this runner. Explicit user \
             disables are still honoured. Unset it to restore the gate."
        );
    }
}

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// The registry's `spawn_path` classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnPath {
    /// A helper spawned to finish the request in flight; dies with it.
    /// Implied-by-task and bounded per the clause.
    InSessionSubagent,
    /// Workflow-style parallel fan-out. Bound declared up front (registry
    /// default 15); on breach the caller degrades to sequential rather than
    /// hard-failing mid-task. Enforced by [`authorize_fanout_spawn`] — see
    /// "Parallel fan-out bound" in the module docs.
    ParallelFanout,
    /// A spawn that OUTLIVES the request: gate continuations, auto-dispatch,
    /// shepherd/steward spawns, `/sessions/spawn` continuations, looping
    /// agents. **Default OFF for a fresh user** — requires an explicitly
    /// enabled registry row.
    StandingContinuation,
}

impl SpawnPath {
    /// The wire value used by `coord.agent_registry.spawn_path`. Also doubles
    /// as the per-path pseudo-agent name used for path-level opt-in rows (see
    /// [`lookup_row`]).
    pub fn as_wire(self) -> &'static str {
        match self {
            SpawnPath::InSessionSubagent => "in_session_subagent",
            SpawnPath::ParallelFanout => "parallel_fanout",
            SpawnPath::StandingContinuation => "standing_continuation",
        }
    }

    /// True for classes whose spawn outlives the request, i.e. the ones the
    /// clause makes a standing per-path opt-in.
    fn outlives_request(self) -> bool {
        matches!(self, SpawnPath::StandingContinuation)
    }
}

/// What the user chose to happen when they disabled an agent.
///
/// Per the clause, disabling a `policy_required` agent is NOT an escalation —
/// the user picks the disposition at disable time, and the session must
/// "[h]onour the recorded disposition and say which one applied."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Block the work outright.
    Block,
    /// The main session does the work itself — no spawn, no cost, gate kept.
    Degrade,
    /// Warn and proceed without the agent.
    WarnProceed,
}

impl Disposition {
    fn from_wire(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "block" => Some(Disposition::Block),
            "degrade" | "degrade_to_inline" => Some(Disposition::Degrade),
            "warn_proceed" | "warn" => Some(Disposition::WarnProceed),
            _ => None,
        }
    }

    fn as_wire(self) -> &'static str {
        match self {
            Disposition::Block => "block",
            Disposition::Degrade => "degrade",
            Disposition::WarnProceed => "warn_proceed",
        }
    }
}

/// The authorization outcome for one spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnDecision {
    /// Spawn.
    Allow,
    /// Spawn — but only after queueing behind the declared `parallel_fanout`
    /// bound, i.e. the clause's *degrade to sequential*. The work proceeds; it
    /// simply did not run in parallel. **Not a refusal:** [`Self::allows_spawn`]
    /// is true and [`Self::refusal`] is `None`, so a call site that predates
    /// this variant cannot mistake a bound breach for a denial — the direction
    /// the clause mandates ("never hard-fail mid-task").
    ///
    /// `in_flight` is the occupancy observed at the moment admission was
    /// refused, i.e. before this spawn queued.
    SerializeToBound { bound: u32, in_flight: u32 },
    /// Do not spawn; the requesting session performs the work inline instead.
    DegradeToInline { reason: String },
    /// Spawn, but surface the warning prominently.
    Warn { reason: String },
    /// Do not spawn; the work is blocked.
    Deny { reason: String },
}

impl SpawnDecision {
    /// True when the caller may go ahead and spawn.
    pub fn allows_spawn(&self) -> bool {
        matches!(
            self,
            SpawnDecision::Allow
                | SpawnDecision::Warn { .. }
                | SpawnDecision::SerializeToBound { .. }
        )
    }

    /// The reason text for any outcome that carries one, for surfacing to the
    /// caller. `None` for `Allow` and for `SerializeToBound` — both authorize
    /// the spawn, and the fan-out numbers travel in the variant's own fields.
    pub fn reason(&self) -> Option<&str> {
        match self {
            SpawnDecision::Allow | SpawnDecision::SerializeToBound { .. } => None,
            SpawnDecision::DegradeToInline { reason }
            | SpawnDecision::Warn { reason }
            | SpawnDecision::Deny { reason } => Some(reason),
        }
    }

    /// A structured, machine-greppable refusal line for the call sites that
    /// must surface the decision as an error. `None` when the spawn may
    /// proceed (`Allow` / `Warn`).
    pub fn refusal(&self) -> Option<String> {
        if self.allows_spawn() {
            return None;
        }
        self.reason()
            .map(|r| format!("spawn-authorization {}: {r}", self.label()))
    }

    /// Stable label for logs, error payloads, and tests.
    pub fn label(&self) -> &'static str {
        match self {
            SpawnDecision::Allow => "allow",
            SpawnDecision::SerializeToBound { .. } => "serialize_to_bound",
            SpawnDecision::DegradeToInline { .. } => "degrade_to_inline",
            SpawnDecision::Warn { .. } => "warn_proceed",
            SpawnDecision::Deny { .. } => "deny",
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One row of `GET /coord/agent-registry/effective`.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryAgent {
    pub agent_name: String,
    #[serde(default)]
    pub purpose: Option<String>,
    /// The class this row governs. A row that declares a class only governs
    /// spawns of that class; `None` governs any class (a class-agnostic row).
    #[serde(default)]
    pub spawn_path: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    /// Whether a disable of this agent must still leave a gate behind.
    #[serde(default)]
    pub policy_required: bool,
    /// Declared parallel fan-out bound. Logged with every decision and
    /// enforced by [`authorize_fanout_spawn`]; absent means
    /// [`DEFAULT_FANOUT_BOUND`].
    #[serde(default)]
    pub fanout_bound: Option<u32>,
    /// **Defaults to `false` when absent.** The contract always sends it; a row
    /// that somehow arrives without it is treated as disabled-with-no-
    /// disposition, i.e. degrade-to-inline — the fail-safe direction.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub disposition: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct EffectiveRegistry {
    #[serde(default)]
    agents: Vec<RegistryAgent>,
}

// ---------------------------------------------------------------------------
// Decision core (pure — no HTTP, no clock, no globals)
// ---------------------------------------------------------------------------

/// What the resolver managed to obtain, as seen by [`decide`].
#[derive(Debug)]
pub(crate) enum Resolution<'a> {
    /// A snapshot within TTL.
    Fresh(&'a [RegistryAgent]),
    /// The refresh failed, but a usable last-known-good snapshot is in hand.
    Stale {
        agents: &'a [RegistryAgent],
        error: &'a str,
        age: Duration,
    },
    /// No usable snapshot: never resolved, or the snapshot aged out.
    Unresolved { error: &'a str },
    /// coord answered 404 — this coord build has no registry endpoint.
    NotDeployed,
    /// This runner holds no device JWT, so it has no coord tenant and there is
    /// no recorded preference to honour.
    Unpaired,
}

/// A decision plus the provenance the clause requires us to state.
#[derive(Debug, Clone)]
pub(crate) struct Verdict {
    pub decision: SpawnDecision,
    /// Which matrix rule fired. Stable identifiers — logged, and asserted on
    /// by the tests.
    pub rule: &'static str,
    /// Which disposition fired, when one did.
    pub disposition: Option<Disposition>,
    /// How old the deciding snapshot was, when it was a stale one.
    pub stale_for: Option<Duration>,
    pub policy_required: bool,
    pub fanout_bound: Option<u32>,
    /// `agent_name` of the row that supplied `fanout_bound`; `None` when no row
    /// governed the spawn. This — not the requested agent name — is what keys
    /// the fan-out lane, so several agents falling back to one class row share
    /// that row's bound instead of getting one bound each. See [`fanout_key`].
    pub bound_source: Option<String>,
}

impl Verdict {
    fn new(decision: SpawnDecision, rule: &'static str) -> Self {
        Verdict {
            decision,
            rule,
            disposition: None,
            stale_for: None,
            policy_required: false,
            fanout_bound: None,
            bound_source: None,
        }
    }
}

/// Pure form of the floor lookup: `raw` is the env override when set.
fn floor_matches(raw: Option<&str>, name: &str) -> bool {
    match raw {
        Some(list) => list
            .split(',')
            .map(str::trim)
            .any(|s| !s.is_empty() && s.eq_ignore_ascii_case(name)),
        None => DEFAULT_POLICY_REQUIRED_FLOOR
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name)),
    }
}

/// True when `agent_name` is on the cold-start `policy_required` floor.
fn on_policy_required_floor(agent_name: Option<&str>) -> bool {
    let Some(name) = agent_name else {
        return false;
    };
    let raw = std::env::var("QONTINUI_SPAWN_AUTHZ_POLICY_REQUIRED_FLOOR").ok();
    floor_matches(raw.as_deref(), name)
}

/// Resolve the row governing this spawn.
///
/// Two-step chain: the named agent's own row first, then the per-path
/// pseudo-agent row (`in_session_subagent` / `parallel_fanout` /
/// `standing_continuation`). The second step is what makes the clause's
/// "standing **per-path** opt-in" expressible for the many funnel sites that
/// have no agent name at all (a gate continuation carries a `gate_id`, not an
/// agent), and it lets a tenant opt a whole class in or out with one row.
///
/// **Class fence:** a row that declares a DIFFERENT `spawn_path` does not govern
/// this spawn. Without it, `{agent_name:"steward",
/// spawn_path:"in_session_subagent", enabled:true}` would authorize a STANDING
/// spawn of `steward` — an ordinary in-session opt-in silently implying the
/// standing one, which is exactly what "never implied by a task" forbids. A row
/// with no declared class is class-agnostic and still governs.
///
/// The fence is part of the SELECTION, not a post-filter on an
/// already-chosen row. `(agent_name, spawn_path)` is the registry's natural key
/// — one agent legitimately has one row per class — so picking "the first row
/// with this name" and then rejecting it for a class mismatch would skip the
/// agent's OWN correctly-classed row and fall through to the class default.
/// For a standing spawn that means silently denying the exact opt-in the user
/// recorded.
fn lookup_row<'a>(
    rows: &'a [RegistryAgent],
    agent_name: Option<&str>,
    path: SpawnPath,
) -> Option<&'a RegistryAgent> {
    if let Some(name) = agent_name {
        if let Some(row) = find_governing(rows, name, path) {
            return Some(row);
        }
    }
    find_governing(rows, path.as_wire(), path)
}

/// Does this row govern spawns of `path`? A row with no declared (or blank)
/// `spawn_path` is class-agnostic and governs every class.
fn governs(row: &RegistryAgent, path: SpawnPath) -> bool {
    row.spawn_path
        .as_deref()
        .map(str::trim)
        .filter(|sp| !sp.is_empty())
        .is_none_or(|sp| sp.eq_ignore_ascii_case(path.as_wire()))
}

/// First row matching `name` that also governs `path`. Exact-name matches are
/// preferred over case-insensitive ones, and within each pass a row that
/// DECLARES the class beats a class-agnostic one — an explicit per-class row is
/// a more specific statement of intent than a catch-all.
fn find_governing<'a>(
    rows: &'a [RegistryAgent],
    name: &str,
    path: SpawnPath,
) -> Option<&'a RegistryAgent> {
    let exact = |r: &&RegistryAgent| r.agent_name == name;
    let loose = |r: &&RegistryAgent| r.agent_name.eq_ignore_ascii_case(name);
    let declared = |r: &&RegistryAgent| {
        r.spawn_path
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    };

    rows.iter()
        .find(|r| exact(r) && declared(r) && governs(r, path))
        .or_else(|| rows.iter().find(|r| exact(r) && governs(r, path)))
        .or_else(|| {
            rows.iter()
                .find(|r| loose(r) && declared(r) && governs(r, path))
        })
        .or_else(|| rows.iter().find(|r| loose(r) && governs(r, path)))
}

fn describe(agent_name: Option<&str>, path: SpawnPath) -> String {
    match agent_name {
        Some(n) => format!("agent `{n}` ({})", path.as_wire()),
        None => format!("spawn class `{}`", path.as_wire()),
    }
}

/// The entire fail-safe matrix, as a pure function. See the module docs for
/// the table this implements.
pub(crate) fn decide(
    resolution: Resolution<'_>,
    agent_name: Option<&str>,
    path: SpawnPath,
) -> Verdict {
    match resolution {
        Resolution::NotDeployed => {
            Verdict::new(SpawnDecision::Allow, "registry-endpoint-not-deployed")
        }
        Resolution::Unpaired => Verdict::new(SpawnDecision::Allow, "runner-unpaired-no-tenant"),
        Resolution::Unresolved { error } => decide_unresolved(error, agent_name, path),
        Resolution::Fresh(rows) => decide_from_rows(rows, agent_name, path, None),
        Resolution::Stale { agents, error, age } => {
            let mut v = decide_from_rows(agents, agent_name, path, Some(age));
            v.decision = annotate(
                v.decision,
                &format!(
                    "registry refresh failed ({error}); decided from the last known good \
                     snapshot, {}s old",
                    age.as_secs()
                ),
            );
            v
        }
    }
}

/// No usable snapshot (never resolved, or aged past [`MAX_SNAPSHOT_AGE`]).
fn decide_unresolved(error: &str, agent_name: Option<&str>, path: SpawnPath) -> Verdict {
    let who = describe(agent_name, path);
    if path.outlives_request() {
        return Verdict::new(
            SpawnDecision::Deny {
                reason: format!(
                    "{who} is a standing spawn (outlives the request) and the agent registry \
                     could not be resolved ({error}), so the standing opt-in cannot be \
                     confirmed. Standing spawns are default-OFF; enable a \
                     `standing_continuation` row in the agent registry to opt in."
                ),
            },
            "unresolvable-standing-continuation-default-off",
        );
    }
    if on_policy_required_floor(agent_name) {
        let mut v = Verdict::new(
            SpawnDecision::DegradeToInline {
                reason: format!(
                    "{who} is policy-required and the agent registry could not be resolved \
                     ({error}). Degrading to inline: no spawn will occur and the requesting \
                     session performs the work itself rather than skipping the gate."
                ),
            },
            "unresolvable-policy-required-degrade",
        );
        v.policy_required = true;
        v
    } else {
        Verdict::new(SpawnDecision::Allow, "unresolvable-optional-allow")
    }
}

/// A snapshot is in hand (fresh, or stale with `stale_for = Some(age)`).
fn decide_from_rows(
    rows: &[RegistryAgent],
    agent_name: Option<&str>,
    path: SpawnPath,
    stale_for: Option<Duration>,
) -> Verdict {
    let who = describe(agent_name, path);
    let Some(row) = lookup_row(rows, agent_name, path) else {
        let mut v = if path.outlives_request() {
            Verdict::new(
                SpawnDecision::Deny {
                    reason: format!(
                        "{who} is a standing spawn (outlives the request) and has no enabled \
                         agent-registry row for that class. Standing spawns are default-OFF \
                         for a fresh user; opt in by enabling a `standing_continuation` row \
                         in the agent registry."
                    ),
                },
                "no-row-standing-continuation-default-off",
            )
        } else {
            Verdict::new(SpawnDecision::Allow, "no-row-legacy-default-allow")
        };
        v.stale_for = stale_for;
        return v;
    };

    let mut v = if row.enabled {
        Verdict::new(SpawnDecision::Allow, "registry-row-enabled")
    } else {
        let disposition = row.disposition.as_deref().and_then(Disposition::from_wire);
        match disposition {
            Some(Disposition::Block) => {
                let mut v = Verdict::new(
                    SpawnDecision::Deny {
                        reason: format!(
                            "{who} is disabled in the agent registry with disposition `block`; \
                             the work is blocked."
                        ),
                    },
                    "registry-row-disabled-disposition",
                );
                v.disposition = Some(Disposition::Block);
                v
            }
            Some(Disposition::Degrade) => {
                let mut v = Verdict::new(
                    SpawnDecision::DegradeToInline {
                        reason: format!(
                            "{who} is disabled in the agent registry with disposition \
                             `degrade`; no spawn will occur and the requesting session \
                             performs the work inline instead."
                        ),
                    },
                    "registry-row-disabled-disposition",
                );
                v.disposition = Some(Disposition::Degrade);
                v
            }
            Some(Disposition::WarnProceed) => {
                let mut v = Verdict::new(
                    SpawnDecision::Warn {
                        reason: format!(
                            "{who} is disabled in the agent registry with disposition \
                             `warn_proceed`; proceeding without it."
                        ),
                    },
                    "registry-row-disabled-disposition",
                );
                v.disposition = Some(Disposition::WarnProceed);
                v
            }
            // "A disable arriving with NO recorded disposition (config import,
            // programmatic toggle) falls back to degrade — the only option that
            // both honours the cost decision and keeps the gate."
            None => {
                let mut v = Verdict::new(
                    SpawnDecision::DegradeToInline {
                        reason: format!(
                            "{who} is disabled in the agent registry with no recorded \
                             disposition; falling back to `degrade` (honours the cost \
                             decision, keeps the gate). No spawn will occur and the \
                             requesting session performs the work inline instead."
                        ),
                    },
                    "registry-row-disabled-no-disposition-fallback",
                );
                v.disposition = Some(Disposition::Degrade);
                v
            }
        }
    };
    v.stale_for = stale_for;
    v.policy_required = row.policy_required;
    v.fanout_bound = row.fanout_bound;
    v.bound_source = Some(row.agent_name.clone());
    v
}

/// Append staleness provenance to a non-`Allow` reason.
fn annotate(decision: SpawnDecision, note: &str) -> SpawnDecision {
    match decision {
        SpawnDecision::Allow => SpawnDecision::Allow,
        // `decide` never produces this variant (it is minted by the fan-out
        // gate, after the matrix has already run), so there is nothing to
        // annotate.
        d @ SpawnDecision::SerializeToBound { .. } => d,
        SpawnDecision::DegradeToInline { reason } => SpawnDecision::DegradeToInline {
            reason: format!("{reason} [{note}]"),
        },
        SpawnDecision::Warn { reason } => SpawnDecision::Warn {
            reason: format!("{reason} [{note}]"),
        },
        SpawnDecision::Deny { reason } => SpawnDecision::Deny {
            reason: format!("{reason} [{note}]"),
        },
    }
}

// ---------------------------------------------------------------------------
// TTL cache
// ---------------------------------------------------------------------------

/// One attempt's outcome, separated from the cache so tests can drive the TTL
/// without HTTP.
#[derive(Debug)]
pub(crate) enum Fetched {
    Ok(Vec<RegistryAgent>),
    /// coord has no registry endpoint (an AUTHENTICATED 404).
    NotDeployed,
    /// This runner holds no device JWT — no request was issued.
    Unpaired,
    Err(String),
}

/// Why no registry rows are available, when none are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Posture {
    #[default]
    Available,
    NotDeployed,
    Unpaired,
}

/// What the cache handed back for one `authorize_spawn` call.
pub(crate) struct Resolved {
    agents: Option<Arc<Vec<RegistryAgent>>>,
    /// Age of `agents` when it is a stale snapshot rather than a fresh one.
    age: Option<Duration>,
    error: Option<String>,
    posture: Posture,
}

impl Resolved {
    pub(crate) fn as_resolution(&self) -> Resolution<'_> {
        match (&self.agents, &self.error) {
            (Some(a), None) => Resolution::Fresh(a),
            (Some(a), Some(e)) => Resolution::Stale {
                agents: a,
                error: e,
                age: self.age.unwrap_or_default(),
            },
            (None, _) => match self.posture {
                Posture::NotDeployed => Resolution::NotDeployed,
                Posture::Unpaired => Resolution::Unpaired,
                Posture::Available => Resolution::Unresolved {
                    error: self
                        .error
                        .as_deref()
                        .unwrap_or("agent registry has never been resolved"),
                },
            },
        }
    }
}

#[derive(Default)]
struct CacheInner {
    /// When the last SUCCESSFUL resolve landed; drives TTL freshness and the
    /// snapshot's age.
    fetched_at: Option<Instant>,
    /// When the last attempt (success or failure) was made; drives the error
    /// backoff so an outage does not make every spawn pay the HTTP timeout.
    attempted_at: Option<Instant>,
    /// Last known good — freshness expires first, then the snapshot itself at
    /// [`MAX_SNAPSHOT_AGE`].
    agents: Option<Arc<Vec<RegistryAgent>>>,
    last_error: Option<String>,
    posture: Posture,
    /// Attempt counter, for the TTL tests.
    attempts: u32,
}

impl CacheInner {
    /// Age of the held snapshot as of `now`, if one is held.
    fn snapshot_age(&self, now: Instant) -> Option<Duration> {
        self.fetched_at.map(|t| now.saturating_duration_since(t))
    }

    /// Drop a snapshot that has aged past [`MAX_SNAPSHOT_AGE`].
    fn expire_snapshot(&mut self, now: Instant) {
        if self.agents.is_some() && self.snapshot_age(now).is_some_and(|a| a > MAX_SNAPSHOT_AGE) {
            warn!(
                "agent registry: dropping last-known-good snapshot older than {}s — \
                 falling back to the cold fail-safe matrix",
                MAX_SNAPSHOT_AGE.as_secs()
            );
            self.agents = None;
            self.fetched_at = None;
        }
    }

    /// A `Resolved` carrying whatever snapshot we still hold plus `error`.
    fn stale_or_unresolved(&self, now: Instant, error: Option<String>) -> Resolved {
        Resolved {
            agents: self.agents.clone(),
            age: self.snapshot_age(now),
            error: error.or_else(|| self.last_error.clone()),
            posture: Posture::Available,
        }
    }
}

/// Hourly rate limiter for the `NotDeployed` `error!`. Returns true at most
/// once an hour per process.
fn not_deployed_log_due(now: Instant) -> bool {
    use std::sync::Mutex;
    static LAST: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));
    const EVERY: Duration = Duration::from_secs(3600);

    let mut last = match LAST.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    match *last {
        Some(t) if now.saturating_duration_since(t) < EVERY => false,
        _ => {
            *last = Some(now);
            true
        }
    }
}

pub(crate) struct RegistryCache {
    ttl: Duration,
    error_backoff: Duration,
    inner: Mutex<CacheInner>,
}

impl RegistryCache {
    pub(crate) fn new(ttl: Duration, error_backoff: Duration) -> Self {
        RegistryCache {
            ttl,
            error_backoff,
            inner: Mutex::new(CacheInner::default()),
        }
    }

    /// Number of fetch attempts made so far (tests).
    #[cfg(test)]
    pub(crate) async fn attempts(&self) -> u32 {
        self.inner.lock().await.attempts
    }

    /// Resolve the registry as of `now`, fetching only when the snapshot is
    /// stale and the error backoff has elapsed.
    ///
    /// The lock is held across the fetch on purpose: it collapses a burst of
    /// concurrent spawns into one coord round-trip instead of a thundering
    /// herd, at the cost of those spawns queueing behind it (bounded by
    /// [`HTTP_TIMEOUT`]). There is no re-entrancy — [`fetch_effective`] never
    /// calls back into [`authorize_spawn`] — so this cannot deadlock, and a
    /// cancelled caller drops the guard cleanly.
    pub(crate) async fn resolve_at<F, Fut>(&self, now: Instant, fetch: F) -> Resolved
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Fetched>,
    {
        let mut inner = self.inner.lock().await;
        inner.expire_snapshot(now);

        let within_ttl = inner
            .fetched_at
            .is_some_and(|t| now.saturating_duration_since(t) < self.ttl);
        if within_ttl && inner.agents.is_some() {
            return Resolved {
                agents: inner.agents.clone(),
                age: None,
                error: None,
                posture: Posture::Available,
            };
        }
        // A cached `NotDeployed` / `Unpaired` posture is honoured for the TTL
        // too — it is a resolved answer, just not a row-bearing one.
        let posture_cached = inner
            .attempted_at
            .is_some_and(|t| now.saturating_duration_since(t) < self.ttl);
        if posture_cached && inner.posture != Posture::Available && inner.agents.is_none() {
            return Resolved {
                agents: None,
                age: None,
                error: None,
                posture: inner.posture,
            };
        }

        let backing_off = inner.last_error.is_some()
            && inner
                .attempted_at
                .is_some_and(|t| now.saturating_duration_since(t) < self.error_backoff);
        if backing_off {
            return inner.stale_or_unresolved(now, None);
        }

        inner.attempts += 1;
        inner.attempted_at = Some(now);
        match fetch().await {
            Fetched::Ok(rows) => {
                inner.agents = Some(Arc::new(rows));
                inner.fetched_at = Some(now);
                inner.last_error = None;
                inner.posture = Posture::Available;
                Resolved {
                    agents: inner.agents.clone(),
                    age: None,
                    error: None,
                    posture: Posture::Available,
                }
            }
            Fetched::NotDeployed => {
                // coord has DISOWNED the endpoint. Keeping the old snapshot
                // would let a later transport error resurrect rows from a route
                // coord says does not exist, so drop it.
                // Rate-limited: this fires once per TTL on every runner
                // predating the coord endpoint, and the Error/Auto-Fix
                // Monitors read this log. Once an hour is enough to keep the
                // temporary allow-all visible without flooding them.
                if not_deployed_log_due(now) {
                    error!(
                        "agent registry: coord returned 404 for {REGISTRY_PATH} — the registry \
                         endpoint is not deployed on this coord. Spawn authorization is \
                         ALLOW-ALL until it ships; this rollout arm is temporary and must be \
                         removed once the endpoint is live fleet-wide (coord gate \
                         706b53d5-c68e-4b80-baf2-31e71a385cb9)."
                    );
                }
                inner.agents = None;
                inner.fetched_at = None;
                inner.last_error = None;
                inner.posture = Posture::NotDeployed;
                Resolved {
                    agents: None,
                    age: None,
                    error: None,
                    posture: Posture::NotDeployed,
                }
            }
            Fetched::Unpaired => {
                // A snapshot from a previous pairing still reflects a real user
                // preference — prefer it over the no-tenant posture.
                if inner.agents.is_some() {
                    let e = "runner holds no device JWT (unpaired)".to_string();
                    inner.last_error = Some(e.clone());
                    inner.posture = Posture::Available;
                    return inner.stale_or_unresolved(now, Some(e));
                }
                inner.last_error = None;
                inner.posture = Posture::Unpaired;
                Resolved {
                    agents: None,
                    age: None,
                    error: None,
                    posture: Posture::Unpaired,
                }
            }
            Fetched::Err(e) => {
                inner.last_error = Some(e.clone());
                inner.posture = Posture::Available;
                inner.stale_or_unresolved(now, Some(e))
            }
        }
    }
}

static CACHE: Lazy<RegistryCache> =
    Lazy::new(|| RegistryCache::new(REGISTRY_CACHE_TTL, REGISTRY_ERROR_BACKOFF));

/// One reusable client: rebuilding the TLS root store on every refresh is
/// avoidable work, and the connection is warm across refreshes.
static HTTP: Lazy<Option<reqwest::Client>> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .ok()
});

// ---------------------------------------------------------------------------
// coord transport
// ---------------------------------------------------------------------------

/// Strip any `user:pass@` userinfo before a URL reaches a log line.
///
/// `COORD_HTTP_URL` is operator-supplied and the fleet's standing rule is that
/// a URL-embedded credential must never be logged.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let host_end = rest.find('/').unwrap_or(rest.len());
    match rest[..host_end].rfind('@') {
        Some(at) => format!("{scheme}://***@{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// GET the effective registry for this device.
///
/// Auth is the repo's canonical device-JWT path — [`crate::coord_http::coord_get`]
/// → `qontinui_runner_lib::auth::attach_device_auth` — the same bearer the
/// write path presents, so this read feeds the one auth-coverage metric. No
/// token is minted here and none is logged.
///
/// The `have_device_token` pre-check is load-bearing beyond avoiding a pointless
/// request: it guarantees the [`Fetched::NotDeployed`] arm is only ever reached
/// by an AUTHENTICATED 404, so a gateway that 404s unauthenticated probes to
/// hide routes cannot turn the gate into allow-all.
async fn fetch_effective() -> Fetched {
    let base = match crate::mcp::agent_worktrees::coord_http_base() {
        Ok(b) => b,
        Err(e) => return Fetched::Err(format!("no coord base url: {e}")),
    };
    if !crate::coord_http::have_device_token() {
        return Fetched::Unpaired;
    }
    let url = format!("{}{}", base.trim_end_matches('/'), REGISTRY_PATH);
    let safe_url = redact_url(&url);

    let Some(client) = HTTP.as_ref() else {
        return Fetched::Err("build http client failed".to_string());
    };
    let resp = match crate::coord_http::coord_get(client, &url).send().await {
        Ok(r) => r,
        Err(e) => return Fetched::Err(format!("GET {safe_url}: {e}")),
    };
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Fetched::NotDeployed;
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        return Fetched::Err(format!(
            "coord returned {status} for GET {safe_url}: {excerpt}"
        ));
    }
    match resp.json::<EffectiveRegistry>().await {
        Ok(reg) => Fetched::Ok(reg.agents),
        Err(e) => Fetched::Err(format!("decode agent registry: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Parallel fan-out bound
// ---------------------------------------------------------------------------

/// Fan-out bound applied when no governing registry row declares one.
///
/// The clause names this number: *"Parallel fan-out: bound declared up front
/// (registry default 15)."*
pub const DEFAULT_FANOUT_BOUND: u32 = 15;

/// Default wait budget for [`authorize_fanout_spawn`].
///
/// A slot covers a spawn's admission (seconds), so a wait this long means the
/// lane is genuinely wedged rather than busy. It must be BOUNDED: an
/// unbounded wait would hang a caller forever on one spawn, which is the
/// "hard-fail mid-task" the clause forbids wearing a different hat.
///
/// **A caller on a reconcile loop must NOT use this default** — its own tick is
/// already the retry mechanism, so queueing inside the tick stalls every other
/// thing that tick does. Such callers pass a near-zero budget to
/// [`authorize_fanout_spawn_with_budget`] and treat
/// [`FanoutAdmission::SlotUnavailable`] as "try again next tick".
pub const FANOUT_WAIT_BUDGET: Duration = Duration::from_secs(120);

/// Ceiling on a declared fan-out bound.
///
/// A bound above this is indistinguishable from unbounded, and clamping keeps
/// `bound as usize` honest on a 32-bit target where a hostile or fat-fingered
/// `fanout_bound: 4_000_000_000` would otherwise truncate.
const MAX_FANOUT_BOUND: u32 = 1024;

/// The bound governing one spawn: the row's declared bound, else
/// [`DEFAULT_FANOUT_BOUND`].
///
/// Clamped to at least 1. A row declaring `0` means "no parallelism", which is
/// *sequential* — one at a time — not a permanent deadlock; the clause admits
/// no reading in which a bound becomes a hard failure.
fn effective_fanout_bound(declared: Option<u32>) -> u32 {
    declared
        .unwrap_or(DEFAULT_FANOUT_BOUND)
        .clamp(1, MAX_FANOUT_BOUND)
}

/// Accounting key for one fan-out lane.
///
/// The argument is the `agent_name` of the row that SUPPLIED the bound
/// ([`Verdict::bound_source`]), not the agent the caller asked about. That
/// distinction is the whole point: when `worker-a` and `worker-b` both fall
/// back to the single `parallel_fanout` class row, keying on the requested name
/// would give them two independent lanes of 15 — 30 concurrent spawns against a
/// declared bound of 15. Keying on the row makes them share the one lane the
/// row describes. `None` (no row governed) is the class lane.
///
/// Case-folded, because the registry lookup that produced the bound is
/// case-insensitive too — two spellings of one row must not get two lanes.
fn fanout_key(bound_source: Option<&str>) -> String {
    bound_source
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(SpawnPath::ParallelFanout.as_wire())
        .to_ascii_lowercase()
}

/// RAII admission ticket for ONE in-flight `parallel_fanout` spawn.
///
/// The count IS the semaphore's permit count, so the decrement is the permit's
/// `Drop`: it runs on the happy path, on every `?`/early return, and on unwind
/// from a panic. Nothing has to remember to release it. That is the whole
/// reason this is a guard rather than a counter the caller decrements — a
/// leaked count would wedge fan-out for the life of the process, strictly
/// worse than the unenforced bound it replaced.
#[must_use = "hold the FanoutSlot for the lifetime of the spawn — dropping it \
              immediately releases the fan-out slot"]
pub struct FanoutSlot {
    /// Held solely for its `Drop`. Never read.
    _permit: OwnedSemaphorePermit,
    key: String,
    bound: u32,
}

impl FanoutSlot {
    /// The bound this slot was admitted against.
    pub fn bound(&self) -> u32 {
        self.bound
    }

    /// The lane this slot belongs to.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl std::fmt::Debug for FanoutSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FanoutSlot")
            .field("key", &self.key)
            .field("bound", &self.bound)
            .finish()
    }
}

impl Drop for FanoutSlot {
    fn drop(&mut self) {
        // `debug!`, not `info!`: this fires once per dispatch on a busy
        // orchestration run, and the Error/Auto-Fix Monitors read this log.
        tracing::debug!(
            fanout_key = %self.key,
            bound = self.bound,
            "parallel fan-out slot released"
        );
    }
}

/// One lane's permits plus the bound they represent.
struct FanoutLane {
    sem: Arc<Semaphore>,
    /// Total permits `sem` currently carries (available + outstanding).
    /// Tracked so a registry change to `fanout_bound` is reconciled instead of
    /// silently ignored for the life of the process.
    bound: u32,
}

/// Per-lane fan-out accounting.
///
/// A `std::sync::Mutex` (not the module's async one) guards the lane map on
/// purpose: the map is only ever touched for the microseconds it takes to
/// clone an `Arc`, and every `.await` in [`Self::admit`] happens after the
/// guard is dropped.
pub(crate) struct FanoutGate {
    lanes: std::sync::Mutex<HashMap<String, FanoutLane>>,
}

/// Internal result of an admission attempt, before it is dressed as a
/// [`FanoutAdmission`].
#[derive(Debug)]
enum Admit {
    /// A slot was free.
    Now(FanoutSlot),
    /// The bound was full; this spawn queued and was then admitted.
    Serialized {
        slot: FanoutSlot,
        bound: u32,
        in_flight: u32,
        waited: Duration,
    },
    /// The bound was full and stayed full for the whole budget.
    Unavailable { bound: u32, waited: Duration },
}

impl FanoutGate {
    pub(crate) fn new() -> Self {
        FanoutGate {
            lanes: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// How many lanes the map currently holds (tests: eviction).
    #[cfg(test)]
    fn lane_count(&self) -> usize {
        match self.lanes.lock() {
            Ok(g) => g.len(),
            Err(e) => e.into_inner().len(),
        }
    }

    /// The lane keys currently held (tests: eviction).
    #[cfg(test)]
    fn lane_keys(&self) -> Vec<String> {
        match self.lanes.lock() {
            Ok(g) => g.keys().cloned().collect(),
            Err(e) => e.into_inner().keys().cloned().collect(),
        }
    }

    /// The lane for `key`, reconciled to `bound`. Returns the semaphore and the
    /// total permits it actually carries (which can exceed `bound` mid-shrink —
    /// see below).
    fn lane(&self, key: &str, bound: u32) -> (Arc<Semaphore>, u32) {
        // A poisoned lock must never wedge fan-out: recovering the inner value
        // is strictly better than panicking every subsequent spawn.
        let mut lanes = match self.lanes.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        // Opportunistic eviction while the guard is already held. Only lanes
        // that are FULLY idle go — no outstanding slot and no queued waiter
        // (both hold an `Arc` clone, so `strong_count > 1` catches them), and
        // every permit back home. Evicting a live lane would let the next
        // admission mint a SECOND semaphore for the same key and double the
        // bound. Today the map holds one entry; this keeps a future caller that
        // keys by a per-invocation name (a task id, a worktree slug) from
        // growing it for the life of the process.
        lanes.retain(|k, l| {
            k == key
                || Arc::strong_count(&l.sem) > 1
                || l.sem.available_permits() != l.bound as usize
        });
        let lane = lanes.entry(key.to_string()).or_insert_with(|| FanoutLane {
            sem: Arc::new(Semaphore::new(bound as usize)),
            bound,
        });
        match lane.bound.cmp(&bound) {
            std::cmp::Ordering::Less => {
                lane.sem.add_permits((bound - lane.bound) as usize);
                lane.bound = bound;
            }
            std::cmp::Ordering::Greater => {
                // `forget_permits` can only take permits that are AVAILABLE
                // right now, so a shrink while slots are outstanding is
                // partial. Record what actually happened rather than the wish —
                // claiming the smaller bound here would leave the extra permits
                // live forever; the remainder is reclaimed on a later admission
                // once the outstanding slots come back.
                let forgotten = lane.sem.forget_permits((lane.bound - bound) as usize) as u32;
                lane.bound -= forgotten;
            }
            std::cmp::Ordering::Equal => {}
        }
        (Arc::clone(&lane.sem), lane.bound)
    }

    /// Admit one spawn into `key`'s lane, queueing for up to `budget`.
    async fn admit(&self, key: &str, bound: u32, budget: Duration) -> Admit {
        let (sem, total) = self.lane(key, bound);
        // Occupancy BEFORE this spawn. Clamped because a mid-shrink lane can
        // momentarily report more available permits than `total`.
        let in_flight = total.saturating_sub(sem.available_permits().min(total as usize) as u32);

        if let Ok(permit) = Arc::clone(&sem).try_acquire_owned() {
            return Admit::Now(FanoutSlot {
                _permit: permit,
                key: key.to_string(),
                bound: total,
            });
        }

        // ONE line per serialization event — not per tick, and not per poll of
        // the wait. This is the clause's degrade-to-sequential becoming
        // observable, so it is worth a `warn!`.
        warn!(
            fanout_key = %key,
            bound = total,
            in_flight,
            wait_budget_ms = budget.as_millis() as u64,
            "parallel fan-out bound reached — degrading to sequential: this spawn queues \
             for a slot instead of being refused"
        );

        let started = Instant::now();
        match tokio::time::timeout(budget, Arc::clone(&sem).acquire_owned()).await {
            Ok(Ok(permit)) => Admit::Serialized {
                slot: FanoutSlot {
                    _permit: permit,
                    key: key.to_string(),
                    bound: total,
                },
                bound: total,
                in_flight,
                waited: started.elapsed(),
            },
            // The semaphore is never closed, so this is unreachable in
            // practice; treat it exactly like exhaustion rather than panicking.
            Ok(Err(_closed)) => Admit::Unavailable {
                bound: total,
                waited: started.elapsed(),
            },
            Err(_elapsed) => Admit::Unavailable {
                bound: total,
                waited: budget,
            },
        }
    }
}

static FANOUT: Lazy<FanoutGate> = Lazy::new(FanoutGate::new);

/// Outcome of one `parallel_fanout` admission.
#[derive(Debug)]
pub enum FanoutAdmission {
    /// Go ahead and spawn, holding `slot` for the spawn's lifetime.
    ///
    /// `decision` is [`SpawnDecision::Allow`] when a slot was free,
    /// [`SpawnDecision::SerializeToBound`] when this spawn queued behind the
    /// bound first, and [`SpawnDecision::Warn`] when the registry recorded a
    /// `warn_proceed` disposition — the recorded disposition outranks the
    /// serialization note, because the clause requires the session to say which
    /// disposition applied (the serialization is logged either way).
    Admitted {
        decision: SpawnDecision,
        slot: FanoutSlot,
    },
    /// The registry matrix refused the spawn. **Terminal** — retrying cannot
    /// change it.
    Refused(SpawnDecision),
    /// The bound was fully occupied and nothing freed within
    /// [`FANOUT_WAIT_BUDGET`]. **Transient** — retry later; per the clause this
    /// must NOT be turned into a task failure. This is the point the clause
    /// says to escalate from, if sequential progress still cannot complete.
    SlotUnavailable {
        key: String,
        bound: u32,
        waited: Duration,
    },
}

/// Authorize ONE `parallel_fanout` spawn and admit it against the declared
/// fan-out bound, queueing for up to [`FANOUT_WAIT_BUDGET`].
///
/// Use this instead of [`authorize_spawn`] for the `parallel_fanout` class:
/// `authorize_spawn` runs the matrix but does no fan-out accounting, so it
/// cannot honour the bound. **Callers on a reconcile loop want
/// [`authorize_fanout_spawn_with_budget`] instead** — see the budget's docs.
pub async fn authorize_fanout_spawn(agent_name: Option<&str>) -> FanoutAdmission {
    authorize_fanout_spawn_with_budget(agent_name, FANOUT_WAIT_BUDGET).await
}

/// [`authorize_fanout_spawn`] with an explicit queueing budget.
///
/// A caller whose own loop already retries (the orchestration conductor's tick)
/// passes a near-zero budget: queueing INSIDE such a loop stalls every other
/// thing that iteration does — completions, gate registration, the stall
/// fingerprint — for the whole budget, once per queued spawn. Returning
/// [`FanoutAdmission::SlotUnavailable`] immediately and letting the next tick
/// retry is the same degrade-to-sequential with a one-tick granularity instead
/// of a multi-minute freeze.
pub async fn authorize_fanout_spawn_with_budget(
    agent_name: Option<&str>,
    wait_budget: Duration,
) -> FanoutAdmission {
    let path = SpawnPath::ParallelFanout;
    let resolved = CACHE.resolve_at(Instant::now(), fetch_effective).await;
    let verdict = decide(resolved.as_resolution(), agent_name, path);
    log_verdict(&verdict, agent_name, path);

    let decision = apply_break_glass(verdict.decision.clone(), verdict.rule, agent_name, path);
    admit_fanout(&FANOUT, &verdict, decision, wait_budget).await
}

/// The fan-out bound currently governing `agent_name`'s `parallel_fanout`
/// spawns.
///
/// Read-only: it resolves the (cached) registry and runs the matrix, but takes
/// no slot and mints no decision. For call sites that must SIZE a batch up
/// front — the orchestration conductor caps how many workers it dispatches per
/// tick — rather than admit one spawn at a time. Deliberately does not log the
/// verdict: this is called every tick, and the per-spawn path already logs.
pub async fn current_fanout_bound(agent_name: Option<&str>) -> u32 {
    let resolved = CACHE.resolve_at(Instant::now(), fetch_effective).await;
    let verdict = decide(
        resolved.as_resolution(),
        agent_name,
        SpawnPath::ParallelFanout,
    );
    effective_fanout_bound(verdict.fanout_bound)
}

/// The admission half of [`authorize_fanout_spawn_with_budget`], split from the
/// resolve/decide half so it can be driven in tests against a local gate and a
/// synthetic verdict — including the paths that must NOT touch the gate.
async fn admit_fanout(
    gate: &FanoutGate,
    verdict: &Verdict,
    decision: SpawnDecision,
    wait_budget: Duration,
) -> FanoutAdmission {
    // Returns BEFORE the gate is touched: a refused spawn must never consume
    // (or queue for) a slot it is not going to use.
    if !decision.allows_spawn() {
        return FanoutAdmission::Refused(decision);
    }

    let bound = effective_fanout_bound(verdict.fanout_bound);
    let key = fanout_key(verdict.bound_source.as_deref());
    match gate.admit(&key, bound, wait_budget).await {
        Admit::Now(slot) => FanoutAdmission::Admitted { decision, slot },
        Admit::Serialized {
            slot,
            bound,
            in_flight,
            waited,
        } => {
            info!(
                fanout_key = %key,
                bound,
                in_flight,
                waited_ms = waited.as_millis() as u64,
                "parallel fan-out: slot granted after serializing behind the bound"
            );
            // A recorded `warn_proceed` disposition must survive — the clause
            // requires saying which disposition applied. Only a plain `Allow`
            // is upgraded to `SerializeToBound`.
            let decision = if matches!(decision, SpawnDecision::Allow) {
                SpawnDecision::SerializeToBound { bound, in_flight }
            } else {
                decision
            };
            FanoutAdmission::Admitted { decision, slot }
        }
        Admit::Unavailable { bound, waited } => {
            warn!(
                fanout_key = %key,
                bound,
                waited_ms = waited.as_millis() as u64,
                "parallel fan-out: no slot freed within the wait budget — the caller should \
                 retry later; this is NOT a task failure"
            );
            FanoutAdmission::SlotUnavailable { key, bound, waited }
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Authorize one spawn against the tenant's agent registry.
///
/// `agent_name` is the registry key when the call site has one (a named
/// subagent); pass `None` for funnel sites that only know their class (gate
/// continuations, auto-dispatch) — those resolve against the per-path
/// pseudo-agent row.
///
/// Never panics, never returns an error type: every failure direction is a
/// decision in the matrix (see the module docs). Every decision is logged with
/// the rule that fired and the disposition (if any), because the clause
/// requires the session to say which one applied.
pub async fn authorize_spawn(agent_name: Option<&str>, spawn_path: SpawnPath) -> SpawnDecision {
    let resolved = CACHE.resolve_at(Instant::now(), fetch_effective).await;
    let verdict = decide(resolved.as_resolution(), agent_name, spawn_path);
    log_verdict(&verdict, agent_name, spawn_path);
    apply_break_glass(verdict.decision, verdict.rule, agent_name, spawn_path)
}

/// Apply the operator break-glass to a verdict, if it is in scope.
///
/// Applied AFTER the verdict so it can be scoped: it may override the gate's
/// own fail-safe refusals, never a recorded user disable ("a spawn the user has
/// explicitly deselected: never"). Shared by [`authorize_spawn`] and
/// [`authorize_fanout_spawn`] so the two entry points cannot drift on which
/// refusals the flag may lift.
fn apply_break_glass(
    decision: SpawnDecision,
    rule: &'static str,
    agent_name: Option<&str>,
    spawn_path: SpawnPath,
) -> SpawnDecision {
    if !decision.allows_spawn() && authz_disabled() && break_glass_may_override(rule) {
        warn!(
            agent = agent_name.unwrap_or("-"),
            spawn_path = spawn_path.as_wire(),
            rule,
            "spawn authorization BYPASSED by QONTINUI_SPAWN_AUTHZ_DISABLED (fail-safe \
             refusal only; user disables are still honoured)"
        );
        return SpawnDecision::Allow;
    }
    decision
}

/// Edge-trigger for [`log_verdict`].
///
/// Some funnel sites ask on a timer — the looping-agent supervisor ticks every
/// few seconds per agent — so an unconditional `info!`/`warn!` per decision
/// would write the same line thousands of times a day into `runner-tauri.log`,
/// which the Error Monitor and Auto-Fix Monitor read. Log at full volume when
/// the answer CHANGES for a given `(agent, class)`, and at `debug!` while it
/// repeats. Every decision is still logged; only the level moves. Mirrors
/// `looping_agent_supervisor::log_posture_transition`.
fn verdict_changed(agent: &str, path: SpawnPath, rule: &'static str, label: &'static str) -> bool {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static SEEN: Lazy<Mutex<HashMap<(String, &'static str), (&'static str, &'static str)>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    let key = (agent.to_string(), path.as_wire());
    let mut seen = match SEEN.lock() {
        Ok(g) => g,
        // A poisoned lock must never silence the gate's logging.
        Err(e) => e.into_inner(),
    };
    match seen.get(&key) {
        Some(&(prev_rule, prev_label)) if prev_rule == rule && prev_label == label => false,
        _ => {
            seen.insert(key, (rule, label));
            true
        }
    }
}

fn log_verdict(verdict: &Verdict, agent_name: Option<&str>, spawn_path: SpawnPath) {
    let disposition = verdict.disposition.map(Disposition::as_wire).unwrap_or("-");
    let stale_secs = verdict.stale_for.map(|d| d.as_secs());
    if !verdict_changed(
        agent_name.unwrap_or("-"),
        spawn_path,
        verdict.rule,
        verdict.decision.label(),
    ) {
        tracing::debug!(
            agent = agent_name.unwrap_or("-"),
            spawn_path = spawn_path.as_wire(),
            decision = verdict.decision.label(),
            rule = verdict.rule,
            disposition,
            "spawn authorization: unchanged"
        );
        return;
    }
    if matches!(verdict.decision, SpawnDecision::Allow) {
        info!(
            agent = agent_name.unwrap_or("-"),
            spawn_path = spawn_path.as_wire(),
            decision = verdict.decision.label(),
            rule = verdict.rule,
            disposition,
            stale_for_secs = ?stale_secs,
            policy_required = verdict.policy_required,
            fanout_bound = ?verdict.fanout_bound,
            "spawn authorization: allow (rule={}, disposition={})",
            verdict.rule,
            disposition
        );
    } else {
        warn!(
            agent = agent_name.unwrap_or("-"),
            spawn_path = spawn_path.as_wire(),
            decision = verdict.decision.label(),
            rule = verdict.rule,
            disposition,
            stale_for_secs = ?stale_secs,
            policy_required = verdict.policy_required,
            fanout_bound = ?verdict.fanout_bound,
            "spawn authorization: {} (rule={}, disposition={}): {}",
            verdict.decision.label(),
            verdict.rule,
            disposition,
            verdict.decision.reason().unwrap_or("")
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        name: &str,
        enabled: bool,
        disposition: Option<&str>,
        policy_required: bool,
    ) -> RegistryAgent {
        RegistryAgent {
            agent_name: name.to_string(),
            purpose: None,
            spawn_path: None,
            model: None,
            effort: None,
            policy_required,
            fanout_bound: None,
            enabled,
            disposition: disposition.map(str::to_string),
            source: None,
        }
    }

    fn row_for(name: &str, class: &str, enabled: bool) -> RegistryAgent {
        RegistryAgent {
            spawn_path: Some(class.to_string()),
            ..row(name, enabled, None, false)
        }
    }

    // -- the decision matrix ------------------------------------------------

    /// Every (row state × disposition) cell of the resolved half of the matrix,
    /// across all three spawn classes. Cold/stale/not-deployed/unpaired
    /// resolutions have their own tests below.
    #[test]
    fn decision_matrix_resolved() {
        struct Case {
            name: &'static str,
            rows: Vec<RegistryAgent>,
            agent: Option<&'static str>,
            path: SpawnPath,
            want_label: &'static str,
            want_rule: &'static str,
            want_disposition: Option<Disposition>,
        }

        let cases = vec![
            Case {
                name: "enabled row allows an in-session subagent",
                rows: vec![row("code-reviewer", true, None, true)],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "allow",
                want_rule: "registry-row-enabled",
                want_disposition: None,
            },
            Case {
                name: "enabled row allows a standing continuation (the opt-in)",
                rows: vec![row("standing_continuation", true, None, false)],
                agent: None,
                path: SpawnPath::StandingContinuation,
                want_label: "allow",
                want_rule: "registry-row-enabled",
                want_disposition: None,
            },
            Case {
                name: "enabled row allows a parallel fan-out",
                rows: vec![row("parallel_fanout", true, None, false)],
                agent: None,
                path: SpawnPath::ParallelFanout,
                want_label: "allow",
                want_rule: "registry-row-enabled",
                want_disposition: None,
            },
            Case {
                name: "disabled + block denies",
                rows: vec![row("code-reviewer", false, Some("block"), true)],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "deny",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::Block),
            },
            Case {
                name: "disabled + degrade degrades to inline",
                rows: vec![row("code-reviewer", false, Some("degrade"), true)],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "degrade_to_inline",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::Degrade),
            },
            Case {
                name: "disabled + warn_proceed warns and proceeds",
                rows: vec![row("code-reviewer", false, Some("warn_proceed"), true)],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "warn_proceed",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::WarnProceed),
            },
            Case {
                name: "disabled fan-out class honours degrade",
                rows: vec![row("parallel_fanout", false, Some("degrade"), false)],
                agent: None,
                path: SpawnPath::ParallelFanout,
                want_label: "degrade_to_inline",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::Degrade),
            },
            Case {
                name: "disabled fan-out class honours block",
                rows: vec![row("parallel_fanout", false, Some("block"), false)],
                agent: None,
                path: SpawnPath::ParallelFanout,
                want_label: "deny",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::Block),
            },
            Case {
                name: "disabled with NO disposition falls back to degrade",
                rows: vec![row("code-reviewer", false, None, true)],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "degrade_to_inline",
                want_rule: "registry-row-disabled-no-disposition-fallback",
                want_disposition: Some(Disposition::Degrade),
            },
            Case {
                name: "disabled with an UNRECOGNIZED disposition falls back to degrade",
                rows: vec![row("code-reviewer", false, Some("shrug"), true)],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "degrade_to_inline",
                want_rule: "registry-row-disabled-no-disposition-fallback",
                want_disposition: Some(Disposition::Degrade),
            },
            Case {
                name: "disabled optional agent still honours block",
                rows: vec![row("scout", false, Some("block"), false)],
                agent: Some("scout"),
                path: SpawnPath::InSessionSubagent,
                want_label: "deny",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::Block),
            },
            Case {
                name: "disabled standing continuation honours warn_proceed",
                rows: vec![row(
                    "standing_continuation",
                    false,
                    Some("warn_proceed"),
                    false,
                )],
                agent: None,
                path: SpawnPath::StandingContinuation,
                want_label: "warn_proceed",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::WarnProceed),
            },
            Case {
                name: "no row at all: in-session subagent falls back to legacy allow",
                rows: vec![],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "allow",
                want_rule: "no-row-legacy-default-allow",
                want_disposition: None,
            },
            Case {
                name: "no row at all: parallel fan-out falls back to legacy allow",
                rows: vec![],
                agent: None,
                path: SpawnPath::ParallelFanout,
                want_label: "allow",
                want_rule: "no-row-legacy-default-allow",
                want_disposition: None,
            },
            Case {
                name: "no row at all: standing continuation is default-OFF",
                rows: vec![],
                agent: None,
                path: SpawnPath::StandingContinuation,
                want_label: "deny",
                want_rule: "no-row-standing-continuation-default-off",
                want_disposition: None,
            },
            Case {
                name: "named standing spawn with no named row falls back to the path row",
                rows: vec![row("standing_continuation", true, None, false)],
                agent: Some("merge-train-steward"),
                path: SpawnPath::StandingContinuation,
                want_label: "allow",
                want_rule: "registry-row-enabled",
                want_disposition: None,
            },
            Case {
                name: "the named row wins over the path row",
                rows: vec![
                    row("standing_continuation", true, None, false),
                    row("merge-train-steward", false, Some("block"), false),
                ],
                agent: Some("merge-train-steward"),
                path: SpawnPath::StandingContinuation,
                want_label: "deny",
                want_rule: "registry-row-disabled-disposition",
                want_disposition: Some(Disposition::Block),
            },
            Case {
                name: "an enabled named row beats a BLOCKED path row",
                rows: vec![
                    row("in_session_subagent", false, Some("block"), false),
                    row("runner-instance", true, None, false),
                ],
                agent: Some("runner-instance"),
                path: SpawnPath::InSessionSubagent,
                want_label: "allow",
                want_rule: "registry-row-enabled",
                want_disposition: None,
            },
            Case {
                name: "a row missing `enabled` on the wire reads as disabled → degrade",
                rows: vec![serde_json::from_str::<RegistryAgent>(
                    r#"{"agent_name":"code-reviewer"}"#,
                )
                .expect("row without enabled must still deserialize")],
                agent: Some("code-reviewer"),
                path: SpawnPath::InSessionSubagent,
                want_label: "degrade_to_inline",
                want_rule: "registry-row-disabled-no-disposition-fallback",
                want_disposition: Some(Disposition::Degrade),
            },
        ];

        for c in cases {
            let v = decide(Resolution::Fresh(&c.rows), c.agent, c.path);
            assert_eq!(v.decision.label(), c.want_label, "case: {}", c.name);
            assert_eq!(v.rule, c.want_rule, "case: {}", c.name);
            assert_eq!(v.disposition, c.want_disposition, "case: {}", c.name);
            assert!(
                v.stale_for.is_none(),
                "case: {} must not be flagged stale",
                c.name
            );
            if c.want_label != "allow" {
                assert!(
                    v.decision.reason().is_some_and(|r| !r.is_empty()),
                    "case: {} must carry a reason",
                    c.name
                );
            }
        }
    }

    /// A named row only governs the class it declares. An in-session opt-in
    /// must NOT imply the standing opt-in ("never implied by a task").
    #[test]
    fn a_named_row_does_not_govern_a_different_class() {
        let rows = vec![row_for("merge-train-steward", "in_session_subagent", true)];

        // The in-session class: the row governs, and allows.
        let v = decide(
            Resolution::Fresh(&rows),
            Some("merge-train-steward"),
            SpawnPath::InSessionSubagent,
        );
        assert_eq!(v.decision.label(), "allow");
        assert_eq!(v.rule, "registry-row-enabled");

        // The standing class: the row does NOT govern, no path row exists,
        // so default-OFF applies.
        let v = decide(
            Resolution::Fresh(&rows),
            Some("merge-train-steward"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(
            v.decision.label(),
            "deny",
            "an in-session opt-in must not imply the standing opt-in"
        );
        assert_eq!(v.rule, "no-row-standing-continuation-default-off");

        // A class-agnostic row (no declared spawn_path) still governs any class.
        let agnostic = vec![row("merge-train-steward", true, None, false)];
        let v = decide(
            Resolution::Fresh(&agnostic),
            Some("merge-train-steward"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "allow");

        // A class-mismatched named row falls through to the PATH row, which
        // still governs.
        let mixed = vec![
            row_for("merge-train-steward", "in_session_subagent", true),
            row("standing_continuation", false, Some("block"), false),
        ];
        let v = decide(
            Resolution::Fresh(&mixed),
            Some("merge-train-steward"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.rule, "registry-row-disabled-disposition");
    }

    /// `(agent_name, spawn_path)` is the registry's natural key, so ONE agent
    /// legitimately has one row PER CLASS. The class fence must select the
    /// agent's correctly-classed row, not reject the first one it happens to
    /// find and fall through to the class default — that would silently deny
    /// the exact standing opt-in the user recorded.
    #[test]
    fn one_agent_with_a_row_per_class_resolves_each_class_to_its_own_row() {
        // Deliberately ordered so the NON-matching row is found first.
        let rows = vec![
            row_for("merge-train-steward", "in_session_subagent", true),
            row_for("merge-train-steward", "standing_continuation", true),
        ];
        let v = decide(
            Resolution::Fresh(&rows),
            Some("merge-train-steward"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(
            v.decision.label(),
            "allow",
            "the agent's own standing_continuation row must win, not the class default"
        );
        assert_eq!(v.rule, "registry-row-enabled");

        // ... and the per-class rows are independent: disabling the standing
        // row must not disturb the in-session one.
        let rows = vec![
            row_for("merge-train-steward", "in_session_subagent", true),
            RegistryAgent {
                disposition: Some("block".to_string()),
                ..row_for("merge-train-steward", "standing_continuation", false)
            },
        ];
        let v = decide(
            Resolution::Fresh(&rows),
            Some("merge-train-steward"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.rule, "registry-row-disabled-disposition");
        let v = decide(
            Resolution::Fresh(&rows),
            Some("merge-train-steward"),
            SpawnPath::InSessionSubagent,
        );
        assert_eq!(v.decision.label(), "allow");

        // A DECLARED row beats a class-agnostic one for the same agent.
        let rows = vec![
            row("merge-train-steward", true, None, false),
            RegistryAgent {
                disposition: Some("block".to_string()),
                ..row_for("merge-train-steward", "standing_continuation", false)
            },
        ];
        let v = decide(
            Resolution::Fresh(&rows),
            Some("merge-train-steward"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(
            v.decision.label(),
            "deny",
            "an explicit per-class row is more specific than a catch-all"
        );

        // The PATH pseudo-agent row is class-fenced too: a row NAMED
        // `standing_continuation` but DECLARING another class does not govern.
        let rows = vec![row_for(
            "standing_continuation",
            "in_session_subagent",
            true,
        )];
        let v = decide(
            Resolution::Fresh(&rows),
            None,
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.rule, "no-row-standing-continuation-default-off");
    }

    /// Coord unreachable and no usable snapshot.
    #[test]
    fn decision_matrix_unresolvable_cold() {
        let err = "GET https://coord.qontinui.io/...: connection refused";

        // policy-required (cold-start floor), NON-standing → degrade, never a
        // silent skip.
        let v = decide(
            Resolution::Unresolved { error: err },
            Some("code-reviewer"),
            SpawnPath::InSessionSubagent,
        );
        assert_eq!(v.decision.label(), "degrade_to_inline");
        assert_eq!(v.rule, "unresolvable-policy-required-degrade");
        assert!(v.policy_required);

        // policy-required AND standing → the standing rule wins (deny is the
        // stronger answer, and the standing opt-in is the harder rule).
        let v = decide(
            Resolution::Unresolved { error: err },
            Some("code-reviewer"),
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.rule, "unresolvable-standing-continuation-default-off");

        // optional in-session → allow; a coord outage must not break the fleet.
        let v = decide(
            Resolution::Unresolved { error: err },
            Some("scout"),
            SpawnPath::InSessionSubagent,
        );
        assert_eq!(v.decision.label(), "allow");
        assert_eq!(v.rule, "unresolvable-optional-allow");

        // optional fan-out → allow.
        let v = decide(
            Resolution::Unresolved { error: err },
            None,
            SpawnPath::ParallelFanout,
        );
        assert_eq!(v.decision.label(), "allow");
        assert_eq!(v.rule, "unresolvable-optional-allow");

        // standing continuation → deny; the standing opt-in is never presumed.
        let v = decide(
            Resolution::Unresolved { error: err },
            None,
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.rule, "unresolvable-standing-continuation-default-off");
        assert!(v
            .decision
            .reason()
            .unwrap()
            .contains("standing_continuation"));
    }

    /// Coord unreachable but a last-known-good snapshot is in hand: the
    /// snapshot decides, and the staleness + age are disclosed in the reason.
    #[test]
    fn unresolvable_with_last_known_good_uses_the_snapshot() {
        let rows = vec![row("standing_continuation", true, None, false)];
        let v = decide(
            Resolution::Stale {
                agents: &rows,
                error: "coord 503",
                age: Duration::from_secs(300),
            },
            None,
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "allow", "LKG opt-in survives an outage");
        assert_eq!(v.stale_for, Some(Duration::from_secs(300)));

        let disabled = vec![row("code-reviewer", false, Some("block"), true)];
        let v = decide(
            Resolution::Stale {
                agents: &disabled,
                error: "coord 503",
                age: Duration::from_secs(1234),
            },
            Some("code-reviewer"),
            SpawnPath::InSessionSubagent,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.stale_for, Some(Duration::from_secs(1234)));
        let reason = v.decision.reason().unwrap();
        assert!(
            reason.contains("last known good snapshot"),
            "a stale decision must disclose that it is stale"
        );
        assert!(reason.contains("1234s old"), "and how stale: {reason}");
    }

    /// A coord build with no registry endpoint must not freeze the fleet.
    #[test]
    fn not_deployed_allows_every_class() {
        for path in [
            SpawnPath::InSessionSubagent,
            SpawnPath::ParallelFanout,
            SpawnPath::StandingContinuation,
        ] {
            let v = decide(Resolution::NotDeployed, Some("code-reviewer"), path);
            assert_eq!(v.decision.label(), "allow", "path {}", path.as_wire());
            assert_eq!(v.rule, "registry-endpoint-not-deployed");
        }
    }

    /// An unpaired runner has no tenant, so it has no preference to honour —
    /// denying would invent one and stop local looping agents.
    #[test]
    fn unpaired_runner_allows_every_class() {
        for path in [
            SpawnPath::InSessionSubagent,
            SpawnPath::ParallelFanout,
            SpawnPath::StandingContinuation,
        ] {
            let v = decide(Resolution::Unpaired, None, path);
            assert_eq!(v.decision.label(), "allow", "path {}", path.as_wire());
            assert_eq!(v.rule, "runner-unpaired-no-tenant");
        }
    }

    #[test]
    fn standing_continuation_requires_an_explicitly_enabled_row() {
        // Present but disabled with no disposition → degrade, not allow.
        let rows = vec![row("standing_continuation", false, None, false)];
        let v = decide(
            Resolution::Fresh(&rows),
            None,
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "degrade_to_inline");

        // A row for a DIFFERENT class does not opt the standing class in.
        let rows = vec![row("in_session_subagent", true, None, false)];
        let v = decide(
            Resolution::Fresh(&rows),
            None,
            SpawnPath::StandingContinuation,
        );
        assert_eq!(v.decision.label(), "deny");
        assert_eq!(v.rule, "no-row-standing-continuation-default-off");
    }

    // -- break-glass scoping ------------------------------------------------

    /// The operator break-glass may lift the gate's own fail-safe refusals,
    /// but never a recorded user disable.
    #[test]
    fn break_glass_never_overrides_a_recorded_user_disable() {
        assert!(!break_glass_may_override(
            "registry-row-disabled-disposition"
        ));
        assert!(!break_glass_may_override(
            "registry-row-disabled-no-disposition-fallback"
        ));
        assert!(break_glass_may_override(
            "no-row-standing-continuation-default-off"
        ));
        assert!(break_glass_may_override(
            "unresolvable-standing-continuation-default-off"
        ));
        assert!(break_glass_may_override(
            "unresolvable-policy-required-degrade"
        ));
    }

    // -- misc vocabulary ----------------------------------------------------

    #[test]
    fn spawn_path_wire_values_match_the_coord_contract() {
        assert_eq!(
            SpawnPath::InSessionSubagent.as_wire(),
            "in_session_subagent"
        );
        assert_eq!(SpawnPath::ParallelFanout.as_wire(), "parallel_fanout");
        assert_eq!(
            SpawnPath::StandingContinuation.as_wire(),
            "standing_continuation"
        );
    }

    #[test]
    fn disposition_wire_values_match_the_coord_contract() {
        assert_eq!(Disposition::from_wire("block"), Some(Disposition::Block));
        assert_eq!(
            Disposition::from_wire("degrade"),
            Some(Disposition::Degrade)
        );
        assert_eq!(
            Disposition::from_wire("warn_proceed"),
            Some(Disposition::WarnProceed)
        );
        assert_eq!(
            Disposition::from_wire("WARN_PROCEED"),
            Some(Disposition::WarnProceed)
        );
        assert_eq!(Disposition::from_wire("nonsense"), None);
    }

    #[test]
    fn effective_registry_decodes_the_contract_payload() {
        let body = r#"{"agents":[{
            "agent_name":"code-reviewer","purpose":"pre-PR review",
            "spawn_path":"in_session_subagent","model":"opus","effort":"high",
            "policy_required":true,"fanout_bound":15,"enabled":false,
            "disposition":"degrade","source":"user_pref"}]}"#;
        let reg: EffectiveRegistry = serde_json::from_str(body).expect("decode");
        assert_eq!(reg.agents.len(), 1);
        let a = &reg.agents[0];
        assert_eq!(a.agent_name, "code-reviewer");
        assert!(a.policy_required);
        assert_eq!(a.fanout_bound, Some(15));
        assert!(!a.enabled);
        assert_eq!(a.disposition.as_deref(), Some("degrade"));
        assert_eq!(a.spawn_path.as_deref(), Some("in_session_subagent"));
        assert_eq!(a.source.as_deref(), Some("user_pref"));
    }

    #[test]
    fn allows_spawn_only_for_allow_and_warn() {
        assert!(SpawnDecision::Allow.allows_spawn());
        assert!(SpawnDecision::Warn { reason: "x".into() }.allows_spawn());
        assert!(!SpawnDecision::DegradeToInline { reason: "x".into() }.allows_spawn());
        assert!(!SpawnDecision::Deny { reason: "x".into() }.allows_spawn());
        assert!(SpawnDecision::Allow.refusal().is_none());
        assert!(SpawnDecision::Deny { reason: "x".into() }
            .refusal()
            .unwrap()
            .starts_with("spawn-authorization deny:"));
    }

    #[test]
    fn cold_start_policy_required_floor_covers_the_review_gate() {
        // Default floor (no env override).
        assert!(floor_matches(None, "code-reviewer"));
        assert!(floor_matches(None, "Code-Reviewer"));
        assert!(floor_matches(None, "code_reviewer"), "rename-tolerant");
        assert!(!floor_matches(None, "scout"));
        // Env override replaces the default set entirely...
        assert!(floor_matches(Some("scout, auditor"), "scout"));
        assert!(!floor_matches(Some("scout"), "code-reviewer"));
        // ... and an empty value disables the floor.
        assert!(!floor_matches(Some(""), "code-reviewer"));
        assert!(!floor_matches(Some("  , "), "code-reviewer"));
        // No agent name is never on the floor.
        assert!(!on_policy_required_floor(None));
    }

    #[test]
    fn urls_are_redacted_before_logging() {
        assert_eq!(
            redact_url("https://user:secret@coord.qontinui.io/coord/x"),
            "https://***@coord.qontinui.io/coord/x"
        );
        assert_eq!(
            redact_url("https://coord.qontinui.io/coord/x"),
            "https://coord.qontinui.io/coord/x"
        );
        // A `@` in the PATH is not userinfo and must not be mangled.
        assert_eq!(
            redact_url("https://coord.qontinui.io/a@b"),
            "https://coord.qontinui.io/a@b"
        );
        assert_eq!(redact_url("not-a-url"), "not-a-url");
    }

    // -- cache TTL ----------------------------------------------------------

    #[tokio::test]
    async fn cache_serves_within_ttl_and_refetches_after() {
        let ttl = Duration::from_secs(90);
        let cache = RegistryCache::new(ttl, Duration::from_secs(15));
        let t0 = Instant::now();

        let r = cache
            .resolve_at(t0, || async {
                Fetched::Ok(vec![row("a", true, None, false)])
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Fresh(_)));
        assert_eq!(cache.attempts().await, 1);

        // Inside the TTL: served from cache, no second attempt. The fetcher
        // panics so a cache miss is impossible to miss.
        let r = cache
            .resolve_at(t0 + ttl - Duration::from_millis(1), || async {
                panic!("must not refetch inside the TTL")
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Fresh(_)));
        assert_eq!(cache.attempts().await, 1);

        // At/after the TTL boundary: refetch.
        let r = cache
            .resolve_at(t0 + ttl, || async {
                Fetched::Ok(vec![row("b", true, None, false)])
            })
            .await;
        assert_eq!(cache.attempts().await, 2);
        match r.as_resolution() {
            Resolution::Fresh(rows) => assert_eq!(rows[0].agent_name, "b"),
            other => panic!("expected Fresh, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cache_falls_back_to_last_known_good_on_error_then_backs_off() {
        let ttl = Duration::from_secs(90);
        let backoff = Duration::from_secs(15);
        let cache = RegistryCache::new(ttl, backoff);
        let t0 = Instant::now();

        cache
            .resolve_at(t0, || async {
                Fetched::Ok(vec![row("standing_continuation", true, None, false)])
            })
            .await;

        // TTL expired + coord down → Stale, decided from the LKG snapshot.
        let t1 = t0 + ttl;
        let r = cache
            .resolve_at(t1, || async { Fetched::Err("connection refused".into()) })
            .await;
        match r.as_resolution() {
            Resolution::Stale { agents, error, age } => {
                assert_eq!(agents[0].agent_name, "standing_continuation");
                assert!(error.contains("connection refused"));
                assert_eq!(age, ttl);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
        assert_eq!(cache.attempts().await, 2);

        // Inside the error backoff: no new attempt, still Stale.
        let r = cache
            .resolve_at(t1 + backoff - Duration::from_millis(1), || async {
                panic!("must not refetch inside the error backoff")
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Stale { .. }));
        assert_eq!(cache.attempts().await, 2);

        // Past the backoff: retry, and a success clears the error.
        let r = cache
            .resolve_at(t1 + backoff, || async {
                Fetched::Ok(vec![row("standing_continuation", true, None, false)])
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Fresh(_)));
        assert_eq!(cache.attempts().await, 3);
    }

    /// A snapshot older than [`MAX_SNAPSHOT_AGE`] is dropped: a permanently
    /// failing resolve must not honour day-old preferences forever.
    #[tokio::test]
    async fn last_known_good_expires_at_the_ceiling() {
        let cache = RegistryCache::new(Duration::from_secs(90), Duration::from_secs(15));
        let t0 = Instant::now();
        cache
            .resolve_at(t0, || async {
                Fetched::Ok(vec![row("standing_continuation", true, None, false)])
            })
            .await;

        // Just inside the ceiling: still stale-usable.
        let r = cache
            .resolve_at(t0 + MAX_SNAPSHOT_AGE, || async {
                Fetched::Err("coord 503".into())
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Stale { .. }));

        // Past it: the snapshot is dropped and the cold matrix applies, which
        // for a standing spawn means deny.
        let r = cache
            .resolve_at(t0 + MAX_SNAPSHOT_AGE + Duration::from_secs(1), || async {
                Fetched::Err("coord 503".into())
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Unresolved { .. }));
        let v = decide(r.as_resolution(), None, SpawnPath::StandingContinuation);
        assert_eq!(v.decision.label(), "deny");
    }

    #[tokio::test]
    async fn cold_start_failure_is_unresolved_not_fresh() {
        let cache = RegistryCache::new(Duration::from_secs(90), Duration::from_secs(15));
        let r = cache
            .resolve_at(Instant::now(), || async {
                Fetched::Err("coord 401".into())
            })
            .await;
        match r.as_resolution() {
            Resolution::Unresolved { error } => assert!(error.contains("401")),
            other => panic!("expected Unresolved, got {other:?}"),
        }
        // ... and that must deny a standing continuation.
        let v = decide(r.as_resolution(), None, SpawnPath::StandingContinuation);
        assert_eq!(v.decision.label(), "deny");
    }

    #[tokio::test]
    async fn not_deployed_is_cached_and_disowns_the_old_snapshot() {
        let ttl = Duration::from_secs(90);
        let cache = RegistryCache::new(ttl, Duration::from_secs(15));
        let t0 = Instant::now();

        // A good snapshot first...
        cache
            .resolve_at(t0, || async {
                Fetched::Ok(vec![row("standing_continuation", true, None, false)])
            })
            .await;
        // ... then coord disowns the endpoint.
        let r = cache
            .resolve_at(t0 + ttl, || async { Fetched::NotDeployed })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::NotDeployed));

        // Cached for the TTL.
        let r = cache
            .resolve_at(t0 + ttl + Duration::from_secs(1), || async {
                panic!("must not refetch inside the TTL")
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::NotDeployed));
        assert_eq!(cache.attempts().await, 2);

        // And a LATER transport error must not resurrect the disowned rows.
        let r = cache
            .resolve_at(t0 + ttl * 3, || async { Fetched::Err("coord 503".into()) })
            .await;
        assert!(
            matches!(r.as_resolution(), Resolution::Unresolved { .. }),
            "a 404-disowned endpoint must not leave a usable snapshot behind"
        );
    }

    #[tokio::test]
    async fn unpaired_prefers_a_previous_snapshot_when_one_exists() {
        let ttl = Duration::from_secs(90);
        let cache = RegistryCache::new(ttl, Duration::from_secs(15));
        let t0 = Instant::now();

        // Cold + unpaired → the no-tenant posture.
        let r = cache.resolve_at(t0, || async { Fetched::Unpaired }).await;
        assert!(matches!(r.as_resolution(), Resolution::Unpaired));

        // Once paired, a snapshot lands...
        let r = cache
            .resolve_at(t0 + ttl, || async {
                Fetched::Ok(vec![row(
                    "standing_continuation",
                    false,
                    Some("block"),
                    false,
                )])
            })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Fresh(_)));

        // ... and if the token later disappears, the recorded preference still
        // decides rather than reverting to allow-all.
        let r = cache
            .resolve_at(t0 + ttl * 2, || async { Fetched::Unpaired })
            .await;
        assert!(matches!(r.as_resolution(), Resolution::Stale { .. }));
        let v = decide(r.as_resolution(), None, SpawnPath::StandingContinuation);
        assert_eq!(v.decision.label(), "deny");
    }

    // -- parallel fan-out bound ---------------------------------------------

    /// Short budget for the "nothing will ever free" cases, so a wedged lane
    /// is proven bounded without making the suite slow.
    const T_SHORT: Duration = Duration::from_millis(50);
    /// Budget for the cases where a slot IS released concurrently.
    const T_GRANT: Duration = Duration::from_secs(5);

    /// Take a slot that must be available immediately.
    async fn take_now(gate: &FanoutGate, key: &str, bound: u32) -> FanoutSlot {
        match gate.admit(key, bound, T_SHORT).await {
            Admit::Now(slot) => slot,
            other => panic!("expected an immediate slot on lane `{key}`, got {other:?}"),
        }
    }

    /// The bound-resolution table: what a declared (or absent) `fanout_bound`
    /// actually governs.
    #[test]
    fn effective_fanout_bound_defaults_to_fifteen_and_clamps_to_sequential() {
        struct Case {
            name: &'static str,
            declared: Option<u32>,
            want: u32,
        }

        assert_eq!(
            DEFAULT_FANOUT_BOUND, 15,
            "the clause names 15 as the registry default"
        );

        let cases = [
            Case {
                name: "no registry row / no declared bound → the registry default",
                declared: None,
                want: 15,
            },
            Case {
                name: "a row-specified bound is honoured verbatim",
                declared: Some(3),
                want: 3,
            },
            Case {
                name: "a bound of 1 is already sequential",
                declared: Some(1),
                want: 1,
            },
            Case {
                name: "a bound of 0 means no parallelism → sequential, never a deadlock",
                declared: Some(0),
                want: 1,
            },
            Case {
                name: "a large bound is not clamped down",
                declared: Some(100),
                want: 100,
            },
        ];

        for c in cases {
            assert_eq!(
                effective_fanout_bound(c.declared),
                c.want,
                "case: {}",
                c.name
            );
        }
    }

    /// The bound must survive the matrix: a row's `fanout_bound` reaches the
    /// verdict, and its absence resolves to the registry default.
    #[test]
    fn a_row_declared_fanout_bound_reaches_the_verdict() {
        let declared = vec![RegistryAgent {
            fanout_bound: Some(3),
            ..row_for("parallel_fanout", "parallel_fanout", true)
        }];
        let v = decide(
            Resolution::Fresh(&declared),
            None,
            SpawnPath::ParallelFanout,
        );
        assert_eq!(v.decision.label(), "allow");
        assert_eq!(v.fanout_bound, Some(3));
        assert_eq!(effective_fanout_bound(v.fanout_bound), 3);

        // A named agent's own row carries its own bound.
        let named = vec![RegistryAgent {
            fanout_bound: Some(2),
            ..row_for("worker", "parallel_fanout", true)
        }];
        let v = decide(
            Resolution::Fresh(&named),
            Some("worker"),
            SpawnPath::ParallelFanout,
        );
        assert_eq!(effective_fanout_bound(v.fanout_bound), 2);

        // No row at all: allowed (legacy default) and bounded by the registry
        // default rather than by nothing.
        let v = decide(Resolution::Fresh(&[]), None, SpawnPath::ParallelFanout);
        assert_eq!(v.rule, "no-row-legacy-default-allow");
        assert_eq!(v.fanout_bound, None);
        assert_eq!(
            effective_fanout_bound(v.fanout_bound),
            DEFAULT_FANOUT_BOUND,
            "an undeclared bound must still be a bound"
        );
    }

    /// The whole point of the variant: a breach authorizes the spawn. A call
    /// site that does not know `SerializeToBound` must not read it as a
    /// refusal, because the clause forbids hard-failing mid-task.
    #[test]
    fn serialize_to_bound_authorizes_the_spawn_and_is_not_a_refusal() {
        let d = SpawnDecision::SerializeToBound {
            bound: 2,
            in_flight: 2,
        };
        assert!(d.allows_spawn());
        assert!(
            d.refusal().is_none(),
            "a fan-out bound breach must never surface as a refusal"
        );
        assert!(d.reason().is_none());
        assert_eq!(d.label(), "serialize_to_bound");
        assert_eq!(
            annotate(d.clone(), "stale snapshot"),
            d,
            "staleness annotation has nothing to say about a fan-out breach"
        );
    }

    #[test]
    fn fanout_key_folds_case_and_falls_back_to_the_class() {
        assert_eq!(fanout_key(None), "parallel_fanout");
        assert_eq!(fanout_key(Some("   ")), "parallel_fanout");
        assert_eq!(
            fanout_key(Some("Code-Reviewer")),
            "code-reviewer",
            "two spellings of one agent must not get two lanes, i.e. twice the bound"
        );
    }

    /// Admission under the bound is immediate; the (bound+1)-th spawn
    /// SERIALIZES — it queues for a slot and then proceeds — rather than being
    /// denied.
    #[tokio::test]
    async fn the_bound_plus_first_spawn_serializes_rather_than_being_denied() {
        let gate = FanoutGate::new();
        let a = take_now(&gate, "lane", 2).await;
        let b = take_now(&gate, "lane", 2).await;

        // Release one slot shortly after the third admission starts queueing.
        let (_, admitted) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                drop(a);
            },
            gate.admit("lane", 2, T_GRANT)
        );

        match admitted {
            Admit::Serialized {
                bound, in_flight, ..
            } => {
                assert_eq!(bound, 2);
                assert_eq!(
                    in_flight, 2,
                    "in_flight is the occupancy observed when admission was refused"
                );
            }
            other => panic!("the 3rd spawn under a bound of 2 must serialize, got {other:?}"),
        }
        drop(b);
    }

    /// The registry default end-to-end: 15 admitted immediately, the 16th
    /// serialized.
    #[tokio::test]
    async fn the_default_bound_admits_fifteen_then_serializes() {
        let gate = FanoutGate::new();
        let bound = effective_fanout_bound(None);
        assert_eq!(bound, 15);

        let mut held = Vec::new();
        for _ in 0..bound {
            held.push(take_now(&gate, "default", bound).await);
        }

        let (_, admitted) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                // The trailing `;` is load-bearing: it drops the popped slot
                // HERE, inside the block. As a tail expression the slot would
                // escape into the `join!` tuple, no permit would be released,
                // and this test would sit until `T_GRANT`.
                held.pop();
            },
            gate.admit("default", bound, T_GRANT)
        );
        match admitted {
            Admit::Serialized {
                bound: b,
                in_flight,
                ..
            } => {
                assert_eq!(b, 15);
                assert_eq!(in_flight, 15);
            }
            other => panic!("the 16th spawn must serialize, not be refused: {other:?}"),
        }
    }

    /// The counter must release on EVERY exit path — success, an early `Err`
    /// return, and a panic unwinding through the holder. A leaked count would
    /// wedge fan-out for the life of the process.
    #[tokio::test]
    async fn the_slot_releases_on_success_error_and_panic_paths() {
        let gate = Arc::new(FanoutGate::new());

        // Success path.
        let slot = take_now(&gate, "release", 1).await;
        drop(slot);
        drop(take_now(&gate, "release", 1).await);

        // Error path: a holder that returns Err before dropping explicitly.
        async fn fallible(gate: &FanoutGate) -> Result<(), String> {
            let _slot = take_now(gate, "release", 1).await;
            Err("boom".to_string())
        }
        assert!(fallible(&gate).await.is_err());
        drop(take_now(&gate, "release", 1).await);

        // Panic path: unwind through the guard.
        let g = Arc::clone(&gate);
        let handle = tokio::spawn(async move {
            let _slot = match g.admit("release", 1, T_SHORT).await {
                Admit::Now(s) => s,
                other => panic!("setup: expected an immediate slot, got {other:?}"),
            };
            panic!("simulated failure while holding the fan-out slot");
        });
        assert!(handle.await.is_err(), "the task must have panicked");

        // ... and the slot is free again for a later spawn.
        let slot = take_now(&gate, "release", 1).await;
        assert_eq!(slot.bound(), 1);
        assert_eq!(slot.key(), "release");
    }

    /// A lane that never frees reports `Unavailable` within the budget — the
    /// wait is bounded, never an infinite hang. The caller treats this as
    /// transient, not as a task failure.
    #[tokio::test]
    async fn the_wait_is_bounded_when_no_slot_ever_frees() {
        let gate = FanoutGate::new();
        let held = take_now(&gate, "wedged", 1).await;
        // Measure from OUTSIDE the call: the reported `waited` is the budget by
        // construction on the timeout arm, so asserting on it proves nothing.
        // What must be proven is that the call RETURNS, and returns near the
        // budget rather than hanging.
        let started = Instant::now();
        let outcome = gate.admit("wedged", 1, T_SHORT).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= T_SHORT && elapsed < T_SHORT * 20,
            "the wait must end at ~the budget, not hang or return early: {elapsed:?}"
        );
        match outcome {
            Admit::Unavailable { bound, waited } => {
                assert_eq!(bound, 1);
                assert_eq!(waited, T_SHORT);
            }
            other => panic!("a permanently full lane must time out, got {other:?}"),
        }
        drop(held);
        drop(take_now(&gate, "wedged", 1).await);
    }

    /// Lanes are per bound-source row: filling one must not starve another.
    #[tokio::test]
    async fn lanes_are_independent_per_key() {
        let gate = FanoutGate::new();
        // Wedge alpha (bound 1, one slot out).
        let a = take_now(&gate, "alpha", 1).await;
        assert!(
            matches!(
                gate.admit("alpha", 1, T_SHORT).await,
                Admit::Unavailable { .. }
            ),
            "alpha's own lane is full"
        );

        // ... and WHILE alpha is wedged, beta admits its full bound of 2
        // immediately. Two distinct assertions, so independence is proved
        // directly rather than inferred from an absence of panics.
        let b1 = take_now(&gate, "beta", 2).await;
        let b2 = take_now(&gate, "beta", 2).await;
        assert!(
            matches!(
                gate.admit("beta", 2, T_SHORT).await,
                Admit::Unavailable { bound: 2, .. }
            ),
            "beta enforces its OWN bound, not alpha's"
        );
        drop((a, b1, b2));
    }

    /// A registry change to `fanout_bound` is reconciled onto the live lane
    /// instead of being ignored for the life of the process.
    #[tokio::test]
    async fn a_raised_bound_is_reconciled_onto_the_live_lane() {
        let gate = FanoutGate::new();
        let s1 = take_now(&gate, "grow", 1).await;
        assert!(matches!(
            gate.admit("grow", 1, T_SHORT).await,
            Admit::Unavailable { .. }
        ));

        // The registry raises the bound to 3 — two more slots appear.
        let s2 = take_now(&gate, "grow", 3).await;
        let s3 = take_now(&gate, "grow", 3).await;
        assert!(
            matches!(
                gate.admit("grow", 3, T_SHORT).await,
                Admit::Unavailable { bound: 3, .. }
            ),
            "and the lane then enforces the RAISED bound"
        );
        drop((s1, s2, s3));
    }

    /// Shrinking is honest about what it could actually do: permits that are
    /// OUT cannot be forgotten, so the shrink completes on a later admission
    /// rather than being recorded as if it had already happened.
    #[tokio::test]
    async fn a_lowered_bound_shrinks_partially_then_reclaims_the_rest() {
        let gate = FanoutGate::new();
        let s1 = take_now(&gate, "shrink", 3).await;
        let s2 = take_now(&gate, "shrink", 3).await;
        let s3 = take_now(&gate, "shrink", 3).await;

        // All three slots are out: a shrink to 1 can forget nothing yet.
        let (_, total) = gate.lane("shrink", 1);
        assert_eq!(total, 3, "outstanding permits cannot be forgotten");

        drop(s2);
        drop(s3);
        // The freed permits are reclaimed on the next admission.
        let (_, total) = gate.lane("shrink", 1);
        assert_eq!(total, 1);

        drop(s1);
        let s = take_now(&gate, "shrink", 1).await;
        assert!(
            matches!(
                gate.admit("shrink", 1, T_SHORT).await,
                Admit::Unavailable { bound: 1, .. }
            ),
            "the lane now enforces the lowered bound"
        );
        drop(s);
    }

    /// A row declaring `fanout_bound: 0` is sequential, not a permanent stall.
    #[tokio::test]
    async fn a_zero_bound_is_sequential_not_a_deadlock() {
        let gate = FanoutGate::new();
        let bound = effective_fanout_bound(Some(0));
        let s = take_now(&gate, "zero", bound).await;
        assert_eq!(s.bound(), 1);
        drop(s);
        drop(take_now(&gate, "zero", bound).await);
    }

    // -- the admission seam (`admit_fanout`) --------------------------------

    /// A synthetic verdict: what the matrix would have produced.
    fn verdict_with(bound: Option<u32>, source: Option<&str>) -> Verdict {
        Verdict {
            fanout_bound: bound,
            bound_source: source.map(str::to_string),
            ..Verdict::new(SpawnDecision::Allow, "test")
        }
    }

    /// A refused spawn must never touch the gate. This is the counter-leak
    /// class at its most direct: a slot taken (or queued for) by a spawn that
    /// will not happen is a slot no later spawn ever gets back.
    #[tokio::test]
    async fn a_refusal_never_consumes_a_fanout_slot() {
        let refusals = [
            SpawnDecision::Deny {
                reason: "blocked".to_string(),
            },
            SpawnDecision::DegradeToInline {
                reason: "degrade".to_string(),
            },
        ];

        for decision in refusals {
            let gate = FanoutGate::new();
            let label = decision.label();
            let v = verdict_with(Some(1), None);
            match admit_fanout(&gate, &v, decision, T_SHORT).await {
                FanoutAdmission::Refused(d) => assert_eq!(d.label(), label),
                other => panic!("{label} must be Refused, got {other:?}"),
            }
            // The lane's single slot is still there for a spawn that IS
            // authorized — the refusal consumed nothing.
            drop(take_now(&gate, "parallel_fanout", 1).await);
        }
    }

    /// A plain `Allow` that had to queue is reported as `SerializeToBound`; a
    /// recorded `warn_proceed` disposition outranks it, because the clause
    /// requires the session to say which disposition applied.
    #[tokio::test]
    async fn serialization_upgrades_allow_but_preserves_a_recorded_warn() {
        for (input, want_label) in [
            (SpawnDecision::Allow, "serialize_to_bound"),
            (
                SpawnDecision::Warn {
                    reason: "user disabled with warn_proceed".to_string(),
                },
                "warn_proceed",
            ),
        ] {
            let gate = FanoutGate::new();
            let held = take_now(&gate, "parallel_fanout", 1).await;
            let v = verdict_with(Some(1), None);

            let (_, admission) = tokio::join!(
                async {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    drop(held);
                },
                admit_fanout(&gate, &v, input, T_GRANT)
            );
            match admission {
                FanoutAdmission::Admitted { decision, .. } => {
                    assert_eq!(decision.label(), want_label);
                    assert!(decision.allows_spawn());
                }
                other => panic!("expected Admitted/{want_label}, got {other:?}"),
            }
        }
    }

    /// The lane is keyed by the row that SUPPLIED the bound, not by the agent
    /// the caller asked about. Two agents falling back to one class row must
    /// share that row's bound — otherwise a declared bound of N becomes N per
    /// distinct caller name.
    #[tokio::test]
    async fn the_lane_is_keyed_by_the_row_that_supplied_the_bound() {
        let gate = FanoutGate::new();
        // Both verdicts resolved against the SAME class row, which is what
        // `bound_source` records — even though the callers named different
        // agents.
        let a = verdict_with(Some(1), Some("parallel_fanout"));
        let b = verdict_with(Some(1), Some("parallel_fanout"));

        let held = match admit_fanout(&gate, &a, SpawnDecision::Allow, T_SHORT).await {
            FanoutAdmission::Admitted { slot, .. } => slot,
            other => panic!("first admission must succeed, got {other:?}"),
        };
        match admit_fanout(&gate, &b, SpawnDecision::Allow, T_SHORT).await {
            FanoutAdmission::SlotUnavailable { key, bound, .. } => {
                assert_eq!(key, "parallel_fanout");
                assert_eq!(bound, 1, "one row, one bound — not one bound per caller");
            }
            other => panic!("the second caller must share the row's lane, got {other:?}"),
        }
        drop(held);

        // A verdict with no governing row lands on the class lane too.
        let none = verdict_with(None, None);
        match admit_fanout(&gate, &none, SpawnDecision::Allow, T_SHORT).await {
            FanoutAdmission::Admitted { slot, .. } => {
                assert_eq!(slot.key(), "parallel_fanout");
                assert_eq!(slot.bound(), DEFAULT_FANOUT_BOUND);
            }
            other => panic!("expected Admitted, got {other:?}"),
        }
    }

    /// An absurd declared bound is clamped rather than trusted into
    /// `Semaphore::new`.
    #[test]
    fn an_absurd_declared_bound_is_clamped() {
        assert_eq!(effective_fanout_bound(Some(u32::MAX)), MAX_FANOUT_BOUND);
        assert_eq!(
            effective_fanout_bound(Some(MAX_FANOUT_BOUND)),
            MAX_FANOUT_BOUND
        );
    }

    /// Fully idle lanes are evicted so a caller keying by a per-invocation name
    /// cannot grow the map for the life of the process — but a lane with an
    /// outstanding slot is NEVER evicted (a second semaphore for a live key
    /// would double the bound).
    #[tokio::test]
    async fn idle_lanes_are_evicted_but_live_ones_are_not() {
        let gate = FanoutGate::new();
        let live = take_now(&gate, "live", 1).await;
        drop(take_now(&gate, "idle", 1).await);
        assert_eq!(gate.lane_count(), 2);

        // Any later lane resolution sweeps the fully-idle lane and keeps the
        // live one (whose only slot is still out).
        let _ = gate.lane("live", 1);
        let keys = gate.lane_keys();
        assert!(
            keys.contains(&"live".to_string()),
            "live lane kept: {keys:?}"
        );
        assert!(
            !keys.contains(&"idle".to_string()),
            "fully idle lane evicted: {keys:?}"
        );

        // The live lane still enforces its bound — eviction did not mint a
        // second semaphore behind its back.
        assert!(matches!(
            gate.admit("live", 1, T_SHORT).await,
            Admit::Unavailable { .. }
        ));
        drop(live);
        drop(take_now(&gate, "live", 1).await);
    }
}
