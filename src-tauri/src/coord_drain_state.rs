//! Coord's device drain, as THIS runner sees it — and the one gate every
//! autonomous local spawn passes through.
//!
//! Plan `2026-09-13-drained-runner-never-reaches-idle`, Phase 3 (D1, D2, D3,
//! D7). Coord's `POST /coord/fleet/drain` is the single quiesce lever for a
//! device. Coord enforces it at its own publish seam, but a runner also spawns
//! work coord never publishes — looping agents, stewards, boot resume and
//! restore, the scheduler, the continuation claim-error arm — so the runner
//! reads the drain too and defers those spawns itself (D1). There is no second,
//! runner-local switch: one lever, two enforcement points.
//!
//! ## Where the state comes from (D2)
//!
//! * The 30 s `POST /coord/devices/register` heartbeat response carries
//!   `drain: {state: clear|drained|unreadable, until?}` — no `reason` (the route
//!   is anonymous). [`HeartbeatOutcome::Registered`] feeds it in.
//! * `GET /coord/devices/me/drain` (device JWT) answers
//!   `{device_id, as_of, state, until?, reason?}`. Read once at boot
//!   ([`boot_read`]) before any restore or resume, and on every transition INTO
//!   `drained` so the banner can show the operator's reason. A secondary
//!   instance sends no heartbeat (the primary owns the machine payload), so it
//!   reads this route on the heartbeat's cadence instead.
//!
//! ## The cached state
//!
//! [`CoordDrainState`]:
//! * `Clear` — autonomous spawns run.
//! * `Drained { until, reason }` — autonomous spawns are DEFERRED, never
//!   discarded. An `until` in the past becomes `Clear` on the next read.
//! * `Unknown { since, cause }` — an unreadable/absent/unparseable drain
//!   answer, three missed heartbeats in a row, or no heartbeat tick for three
//!   intervals. FAIL-CLOSED for autonomous spawns (the same posture as coord's
//!   `DrainVerdict::Unreadable`), and never a trigger for wind-down. An absent
//!   field is NOT read as clear — served policy `verification-and-evidence`
//!   `unknown-must-not-render-as-a-default`.
//! * `NotEnrolled { why }` — this runner is not a coord device at all (no
//!   `machine.json`, no coord URL, no tenant binding), so no device drain can
//!   exist for it. Autonomous spawns run. This is a KNOWN state, not an unknown
//!   one: coord has no device row it could drain, and treating a standalone
//!   runner as permanently "unknown" would silently switch its autonomy off.
//!
//! Operator-initiated spawns (the runner UI's own Tauri commands) always pass,
//! whatever the state (D3): an operator sitting at the runner must never find
//! their own "new terminal" button dead. The UI shows a draining banner instead.
//!
//! ## The dispatch role (a second, standing reason to defer)
//!
//! Coord serves the device's operator-chosen dispatch role BESIDE the drain on
//! both reads, as `dispatch_role` (`workhorse` | `bench` | `ci_node`; plan
//! `2026-10-02-fleet-machine-roles-workhorse-bench-ci-node`). A `bench` or
//! `ci_node` role closes the AGENT lane: coord sends that machine no sessions.
//! Coord enforces that on its own dispatch paths, but the runner's LOCAL
//! autonomous paths (boot tab restore, AI-session boot resume, steward
//! restart-after-drain, workflow auto-resume, looping agents, the scheduler)
//! never pass through coord, so the runner closes them itself: every autonomous
//! origin is deferred with [`DeferClass::RoleClosed`] while the last read role
//! closes the agent lane.
//!
//! The role is deliberately NOT folded into [`CoordDrainState`]. `state` stays
//! the drain's answer, so a role never triggers the drained-runner wind-down
//! (coord's `DeviceDrainWire` makes the same split: the sessions an operator
//! started at the box's own terminal are never governed). Operator origins pass
//! as they do under a drain. The CI lane is untouched: CI jobs are not
//! [`SpawnOrigin`]s and never reach this gate.
//!
//! An ABSENT role (an older coord, or coord's role read failing) or a role
//! spelling this build does not know keeps today's behaviour: the drain alone
//! decides. A read that serves the key replaces the cached role; a read that
//! omits it (coord's failed role read, or any failed drain read) keeps it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Tauri event emitted on every state change (and whenever the deferred-work
/// count grows). Payload: [`CoordDrainSnapshot`].
pub const COORD_DRAIN_STATE_EVENT: &str = "coord-drain-state-changed";

/// Missed heartbeats in a row after which the cached state becomes `Unknown`
/// (D2: three × 30 s = 90 s).
pub const MISSED_HEARTBEATS_TO_UNKNOWN: u32 = 3;

/// Timeout for one `GET /coord/devices/me/drain`.
const ME_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Spawn origin (D7) and the drain gate (D3)
// ---------------------------------------------------------------------------

/// Why a session is being spawned — the D7 vocabulary, stamped by the spawner
/// rather than reconstructed later. The wire values are exactly the contract's
/// `intent.spawn_origin` vocabulary (C2); coord rejects anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpawnOrigin {
    /// A gate continuation delivered by coord.
    GateContinuation,
    /// A work-unit continuation delivered by coord.
    UnitContinuation,
    /// Any other coord-dispatched spawn (`spawn_requested`, condition checks,
    /// cross-machine handoffs). Coord's `dispatch_source` names the kind.
    CoordDispatch,
    /// A looping agent's spawn, relaunch or respawn.
    LoopingAgent,
    /// A steward started by anything other than the runner UI.
    Steward,
    /// A coord respawn request materialized on this device.
    Respawn,
    /// A scheduled task.
    Scheduler,
    /// Orchestration-loop fan-out.
    Orchestration,
    /// Boot-time resume or terminal-tab restore.
    BootResume,
    /// The runner UI's "new terminal" (and its other terminal commands).
    OperatorTerminal,
    /// The runner UI's "new chat".
    OperatorChat,
    /// A caller whose reason this runner cannot name (an HTTP or relay door).
    /// Treated as autonomous: fail closed.
    Unknown,
}

impl SpawnOrigin {
    /// Every variant, in declaration order.
    pub const ALL: [SpawnOrigin; 12] = [
        SpawnOrigin::GateContinuation,
        SpawnOrigin::UnitContinuation,
        SpawnOrigin::CoordDispatch,
        SpawnOrigin::LoopingAgent,
        SpawnOrigin::Steward,
        SpawnOrigin::Respawn,
        SpawnOrigin::Scheduler,
        SpawnOrigin::Orchestration,
        SpawnOrigin::BootResume,
        SpawnOrigin::OperatorTerminal,
        SpawnOrigin::OperatorChat,
        SpawnOrigin::Unknown,
    ];

    /// The `intent.spawn_origin` wire value (contract C2).
    pub fn as_wire(self) -> &'static str {
        match self {
            SpawnOrigin::GateContinuation => "gate_continuation",
            SpawnOrigin::UnitContinuation => "unit_continuation",
            SpawnOrigin::CoordDispatch => "coord_dispatch",
            SpawnOrigin::LoopingAgent => "looping_agent",
            SpawnOrigin::Steward => "steward",
            SpawnOrigin::Respawn => "respawn",
            SpawnOrigin::Scheduler => "scheduler",
            SpawnOrigin::Orchestration => "orchestration",
            SpawnOrigin::BootResume => "boot_resume",
            SpawnOrigin::OperatorTerminal => "operator_terminal",
            SpawnOrigin::OperatorChat => "operator_chat",
            SpawnOrigin::Unknown => "unknown",
        }
    }

    /// `false` only for the two operator origins — spawns an operator sitting
    /// at this runner asked for. Everything else, `Unknown` included, is
    /// autonomous and honours the drain.
    pub fn is_autonomous(self) -> bool {
        !matches!(
            self,
            SpawnOrigin::OperatorTerminal | SpawnOrigin::OperatorChat
        )
    }
}

impl std::fmt::Display for SpawnOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// The operator-chosen kind of work coord sends this machine: coord's
/// `DispatchRole` (`qontinui-coord` `dispatch_role.rs`), as served in the
/// `dispatch_role` key beside the drain. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DispatchRole {
    /// Every lane open.
    Workhorse,
    /// No lane open: coord sends no CI and no sessions.
    Bench,
    /// CI only: the agent lane is closed.
    CiNode,
}

impl DispatchRole {
    /// Parse coord's wire spelling. `None` for a spelling this build does not
    /// know, which the caller treats exactly like an absent role.
    pub fn parse(s: &str) -> Option<DispatchRole> {
        match s {
            "workhorse" => Some(DispatchRole::Workhorse),
            "bench" => Some(DispatchRole::Bench),
            "ci_node" => Some(DispatchRole::CiNode),
            _ => None,
        }
    }

    /// Coord's wire spelling.
    pub fn as_wire(self) -> &'static str {
        match self {
            DispatchRole::Workhorse => "workhorse",
            DispatchRole::Bench => "bench",
            DispatchRole::CiNode => "ci_node",
        }
    }

    /// Whether this role closes the AGENT lane: coord's
    /// `lane_open(role, DrainLane::Agent) == false`.
    pub fn closes_agent_lane(self) -> bool {
        matches!(self, DispatchRole::Bench | DispatchRole::CiNode)
    }
}

/// Why the drain deferred a spawn: coord said `drained`, or the state is
/// unknown. Carried on the deferral itself so every surface reporting it (a
/// 409 body, a continuation stamp) names the class the DECISION saw, never a
/// later re-read that may already have moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferClass {
    Drained,
    Unknown,
    /// The device's dispatch role closes the agent lane (`bench` / `ci_node`).
    RoleClosed,
}

impl DeferClass {
    /// The refusal code, matching coord's own 409 vocabulary (contract C5).
    pub fn code(self) -> &'static str {
        match self {
            DeferClass::Drained => "device_drained",
            DeferClass::Unknown => "drain_unreadable",
            // Coord's own refusal code for the same condition
            // (`dispatch_role::ROLE_CLOSES_AGENT_LANE`).
            DeferClass::RoleClosed => "role_closes_agent_lane",
        }
    }

    /// The [`CoordDrainState::label`] of the state that deferred, or
    /// `role_closed` when the dispatch role did.
    pub fn label(self) -> &'static str {
        match self {
            DeferClass::Drained => "drained",
            DeferClass::Unknown => "unknown",
            DeferClass::RoleClosed => "role_closed",
        }
    }
}

/// The drain gate's answer for one spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainGate {
    /// Spawn.
    Allow,
    /// Do not spawn now; leave the work where it will be picked up again once
    /// the drain lifts. `reason` is operator-readable.
    Defer { reason: String, class: DeferClass },
}

impl DrainGate {
    pub fn allows(&self) -> bool {
        matches!(self, DrainGate::Allow)
    }
}

/// The gate for `origin` against the CURRENT cached state and dispatch role.
/// Cheap and synchronous; never performs I/O.
pub fn drain_gate(origin: SpawnOrigin) -> DrainGate {
    let (state, role) = current_with_role();
    gate_with_role_at(&state, role, origin, Utc::now())
}

/// [`drain_gate`], also recording a deferral of `work_key` so the banner's
/// deferred-work count includes it. Use a key that identifies the WORK (a gate
/// id, an agent id, a steward kind), so a supervisor asking every tick counts
/// once.
pub fn drain_gate_for_work(origin: SpawnOrigin, work_key: &str) -> DrainGate {
    let gate = drain_gate(origin);
    if !gate.allows() {
        record_deferral(origin, work_key);
    }
    gate
}

/// Longest caller-supplied component a deferral key may carry verbatim.
/// Anything longer is digested by [`bounded_work_key`].
pub const MAX_WORK_KEY_TAIL: usize = 48;

/// Hard cap on distinct deferral keys held at once (see [`record_deferral`]).
pub const MAX_DEFERRED_KEYS: usize = 512;

/// Build a deferral key whose caller-supplied half is BOUNDED.
///
/// Several doors key their deferral on a string the CALLER chose — a terminal
/// title, a relay `request_id`. Left verbatim, a caller could grow the banner's
/// `deferred` set (and each key's length) without limit simply by varying that
/// string, which is a remote-influenced allocation in a process an operator is
/// watching. The caller-supplied tail is therefore passed through only while it
/// is short and made of key-safe characters; anything else is replaced by a
/// fixed-width digest of it, so the key stays stable per distinct caller value,
/// stays readable for the ordinary case, and can never exceed
/// `prefix.len() + 1 + MAX_WORK_KEY_TAIL`.
///
/// CARDINALITY is bounded separately, by [`MAX_DEFERRED_KEYS`] — a digest keeps
/// each key small but a caller varying its input still produces distinct keys.
pub fn bounded_work_key(prefix: &str, raw: &str) -> String {
    let plain = raw.len() <= MAX_WORK_KEY_TAIL
        && !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'));
    if plain {
        return format!("{prefix}:{raw}");
    }
    // FNV-1a — a stable, dependency-free digest. This is a bounding device, not
    // a security boundary: a collision merges two callers' deferrals into one
    // banner row, which costs a count, not a decision.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in raw.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}:#{hash:016x}")
}

/// PURE: the gate for `origin` against `state`.
pub fn gate_for(state: &CoordDrainState, origin: SpawnOrigin) -> DrainGate {
    gate_for_at(state, origin, Utc::now())
}

/// PURE: [`gate_for`] at `now`. A drain whose `until` has passed is over, so it
/// allows — whether or not a read has folded the expiry in yet.
pub fn gate_for_at(state: &CoordDrainState, origin: SpawnOrigin, now: DateTime<Utc>) -> DrainGate {
    if !origin.is_autonomous() {
        return DrainGate::Allow;
    }
    match state {
        CoordDrainState::Clear | CoordDrainState::NotEnrolled { .. } => DrainGate::Allow,
        CoordDrainState::Drained { until: Some(u), .. } if *u <= now => DrainGate::Allow,
        CoordDrainState::Drained { until, reason } => {
            let until = until
                .map(|u| format!(" until {}", u.to_rfc3339()))
                .unwrap_or_default();
            let reason = reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            DrainGate::Defer {
                reason: format!(
                    "coord has drained this device{until}{reason} — the autonomous {origin} \
                     spawn is deferred and runs once the drain lifts"
                ),
                class: DeferClass::Drained,
            }
        }
        CoordDrainState::Unknown { cause, .. } => DrainGate::Defer {
            reason: format!(
                "coord drain state unknown ({cause}) — autonomous spawns paused, so the \
                 {origin} spawn is deferred until the state is read again"
            ),
            class: DeferClass::Unknown,
        },
    }
}

/// PURE: the dispatch role's half of the gate. Defers an autonomous `origin`
/// when `role` closes the agent lane; allows otherwise, including an absent
/// (`None`) role, which keeps today's behaviour.
pub fn role_gate(role: Option<DispatchRole>, origin: SpawnOrigin) -> DrainGate {
    match role {
        Some(role) if origin.is_autonomous() && role.closes_agent_lane() => DrainGate::Defer {
            // Same shape as the drain reasons: the state, then ONE " — " tail
            // carrying the promise, which `relay_create_drain_message` strips.
            reason: format!(
                "coord's dispatch role for this device is `{}`, which closes the agent \
                 lane — the autonomous {origin} spawn is deferred and runs only if the \
                 role changes",
                role.as_wire()
            ),
            class: DeferClass::RoleClosed,
        },
        _ => DrainGate::Allow,
    }
}

/// PURE: the full gate: the dispatch role, then the drain. The role is checked
/// first because it is the standing fact (coord reports a role over a drain
/// when both close a lane, for the same reason).
pub fn gate_with_role_at(
    state: &CoordDrainState,
    role: Option<DispatchRole>,
    origin: SpawnOrigin,
    now: DateTime<Utc>,
) -> DrainGate {
    match role_gate(role, origin) {
        DrainGate::Allow => gate_for_at(state, origin, now),
        defer => defer,
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The cached drain state. See the module docs for each variant's meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordDrainState {
    Clear,
    Drained {
        until: Option<DateTime<Utc>>,
        /// Served only by `GET /coord/devices/me/drain`; `None` until that
        /// read lands (or when the operator gave none).
        reason: Option<String>,
    },
    Unknown {
        since: DateTime<Utc>,
        cause: String,
    },
    NotEnrolled {
        why: &'static str,
    },
}

impl CoordDrainState {
    /// Stable label: `clear` | `drained` | `unknown` | `not_enrolled`.
    pub fn label(&self) -> &'static str {
        match self {
            CoordDrainState::Clear => "clear",
            CoordDrainState::Drained { .. } => "drained",
            CoordDrainState::Unknown { .. } => "unknown",
            CoordDrainState::NotEnrolled { .. } => "not_enrolled",
        }
    }

    /// Whether AUTONOMOUS spawns may run — `Clear` or `NotEnrolled`.
    pub fn allows_autonomous_spawns(&self) -> bool {
        matches!(
            self,
            CoordDrainState::Clear | CoordDrainState::NotEnrolled { .. }
        )
    }
}

/// One read of coord's drain answer, before it is folded into the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainObservation {
    Clear,
    Drained {
        until: Option<DateTime<Utc>>,
        reason: Option<String>,
    },
    /// Coord answered `state: "unreadable"`.
    Unreadable,
    /// The response carried no drain object, or one this build cannot parse.
    Absent {
        detail: String,
    },
    /// This runner is not a coord device.
    NotEnrolled {
        why: &'static str,
    },
}

#[derive(Debug, Deserialize)]
struct DrainWire {
    state: String,
    #[serde(default)]
    until: Option<DateTime<Utc>>,
    #[serde(default)]
    reason: Option<String>,
}

fn observation_from_wire(value: &serde_json::Value) -> DrainObservation {
    match serde_json::from_value::<DrainWire>(value.clone()) {
        Ok(w) => match w.state.as_str() {
            "clear" => DrainObservation::Clear,
            "drained" => DrainObservation::Drained {
                until: w.until,
                reason: w.reason.filter(|r| !r.trim().is_empty()),
            },
            "unreadable" => DrainObservation::Unreadable,
            other => DrainObservation::Absent {
                detail: format!("unrecognised drain state {other:?}"),
            },
        },
        Err(e) => DrainObservation::Absent {
            detail: format!("drain object did not parse: {e}"),
        },
    }
}

/// What one drain read said about the dispatch role.
///
/// Two different absences, kept apart because coord gives them different
/// meanings (`qontinui-coord` `dispatch_role::device_drain_wire_from`): when
/// coord's ROLE read succeeds it ALWAYS serves a role (`workhorse` for a
/// machine with no row), and it omits the key only when that read FAILED,
/// while still serving a readable drain `state`. So a missing key is a failed
/// read of the role, never "the role is gone": it keeps the cached role. An
/// older coord never sends the key, so its cached role stays `None` and
/// today's behaviour holds exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedRole {
    /// The key is absent (or not a string, or the body did not parse): the
    /// role was not read. Keeps the cached role.
    NotServed,
    /// The key is present: `Some` for a spelling this build knows, `None` for
    /// one it does not (which, like an absent role, keeps today's behaviour).
    Served(Option<DispatchRole>),
}

/// PURE: the `dispatch_role` key of a drain object.
fn role_from_wire(value: &serde_json::Value) -> ServedRole {
    match value.get("dispatch_role").and_then(|v| v.as_str()) {
        Some(s) => ServedRole::Served(DispatchRole::parse(s)),
        None => ServedRole::NotServed,
    }
}

/// PURE: the dispatch role out of a `POST /coord/devices/register` response
/// body (the `dispatch_role` key of its `drain` object).
pub fn parse_register_role(body: &str) -> ServedRole {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| json.get("drain").map(role_from_wire))
        .unwrap_or(ServedRole::NotServed)
}

/// PURE: the dispatch role out of a `GET /coord/devices/me/drain` 200 body.
pub fn parse_me_drain_role(body: &str) -> ServedRole {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(|json| role_from_wire(&json))
        .unwrap_or(ServedRole::NotServed)
}

/// PURE: the `drain` object out of a `POST /coord/devices/register` response
/// body. A body with no `drain` key is `Absent`, never `Clear`.
pub fn parse_register_response(body: &str) -> DrainObservation {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return DrainObservation::Absent {
            detail: "register response is not JSON".to_string(),
        };
    };
    match json.get("drain") {
        Some(drain) if !drain.is_null() => observation_from_wire(drain),
        _ => DrainObservation::Absent {
            detail: "register response carried no `drain` object (coord predates Phase 2?)"
                .to_string(),
        },
    }
}

/// PURE: a `GET /coord/devices/me/drain` 200 body (the drain object flattened
/// beside `device_id` / `as_of`).
pub fn parse_me_drain_response(body: &str) -> DrainObservation {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(json) => observation_from_wire(&json),
        Err(_) => DrainObservation::Absent {
            detail: "me/drain response is not JSON".to_string(),
        },
    }
}

/// What one fold changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Transition {
    /// The state is different from before.
    pub changed: bool,
    /// The state became `Drained` from something else, or its `until` moved —
    /// the caller should read `me/drain` for the reason.
    pub entered_drained: bool,
}

/// PURE state machine over observations and missed reads.
#[derive(Debug, Clone)]
pub struct DrainTracker {
    state: CoordDrainState,
    misses: u32,
}

impl DrainTracker {
    /// The boot state: `Unknown` until the first read lands.
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            state: CoordDrainState::Unknown {
                since: now,
                cause: "coord drain state not read yet".to_string(),
            },
            misses: 0,
        }
    }

    pub fn state(&self) -> &CoordDrainState {
        &self.state
    }

    fn unknown_since(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match &self.state {
            CoordDrainState::Unknown { since, .. } => *since,
            _ => now,
        }
    }

    fn set(&mut self, next: CoordDrainState) -> Transition {
        let entered_drained = match (&self.state, &next) {
            (
                CoordDrainState::Drained { until: a, .. },
                CoordDrainState::Drained { until: b, .. },
            ) => a != b,
            (_, CoordDrainState::Drained { .. }) => true,
            _ => false,
        };
        let changed = self.state != next;
        self.state = next;
        Transition {
            changed,
            entered_drained,
        }
    }

    /// Fold one successful read.
    pub fn observe(&mut self, obs: DrainObservation, now: DateTime<Utc>) -> Transition {
        self.misses = 0;
        let next = match obs {
            DrainObservation::Clear => CoordDrainState::Clear,
            DrainObservation::Drained { until, reason } => {
                if until.is_some_and(|u| u <= now) {
                    // An expired drain is over, whatever the payload still says.
                    CoordDrainState::Clear
                } else {
                    // The heartbeat never carries `reason`; keep the one the
                    // last me/drain read supplied for this same drain.
                    let reason = reason.or_else(|| match &self.state {
                        CoordDrainState::Drained {
                            until: prev_until,
                            reason: prev_reason,
                        } if *prev_until == until => prev_reason.clone(),
                        _ => None,
                    });
                    CoordDrainState::Drained { until, reason }
                }
            }
            DrainObservation::Unreadable => CoordDrainState::Unknown {
                since: self.unknown_since(now),
                cause: "coord answered drain state `unreadable`".to_string(),
            },
            DrainObservation::Absent { detail } => CoordDrainState::Unknown {
                since: self.unknown_since(now),
                cause: detail,
            },
            DrainObservation::NotEnrolled { why } => CoordDrainState::NotEnrolled { why },
        };
        self.set(next)
    }

    /// Fold one FAILED read (heartbeat error, me/drain transport failure).
    /// The third consecutive miss makes the state `Unknown`; an already-unknown
    /// state takes the newest cause.
    pub fn miss(&mut self, cause: &str, now: DateTime<Utc>) -> Transition {
        self.misses = self.misses.saturating_add(1);
        if matches!(self.state, CoordDrainState::Unknown { .. })
            || self.misses >= MISSED_HEARTBEATS_TO_UNKNOWN
        {
            let next = CoordDrainState::Unknown {
                since: self.unknown_since(now),
                cause: format!(
                    "{} consecutive failed drain read(s); last: {cause}",
                    self.misses
                ),
            };
            // A cause-only rewrite of an existing Unknown is not a change worth
            // an event per tick.
            let was_unknown = matches!(self.state, CoordDrainState::Unknown { .. });
            let mut t = self.set(next);
            if was_unknown {
                t.changed = false;
            }
            return t;
        }
        Transition::default()
    }
}

/// PURE: apply the no-tick staleness rule. A tracker that has not been folded
/// for `stale_after` reads as `Unknown` — a wedged heartbeat must not leave a
/// stale `Clear` in force.
pub fn effective_state(
    tracked: &CoordDrainState,
    last_fold: Instant,
    last_fold_at: DateTime<Utc>,
    stale_after: Duration,
    now: Instant,
    now_utc: DateTime<Utc>,
) -> CoordDrainState {
    if matches!(tracked, CoordDrainState::Unknown { .. }) {
        return tracked.clone();
    }
    // An expired drain is over the moment its deadline passes, not a heartbeat
    // later.
    if let CoordDrainState::Drained { until: Some(u), .. } = tracked {
        if *u <= now_utc {
            return CoordDrainState::Clear;
        }
    }
    let age = now.saturating_duration_since(last_fold);
    if age > stale_after {
        return CoordDrainState::Unknown {
            since: last_fold_at,
            cause: format!(
                "no drain read for {}s (the heartbeat is not ticking)",
                age.as_secs()
            ),
        };
    }
    tracked.clone()
}

// ---------------------------------------------------------------------------
// Process-global cache
// ---------------------------------------------------------------------------

struct Inner {
    tracker: DrainTracker,
    last_fold: Instant,
    last_fold_at: DateTime<Utc>,
    /// Distinct deferred work items since the drain began, `(origin, key)`.
    deferred: BTreeSet<(SpawnOrigin, String)>,
    /// Whether `deferred` has hit [`MAX_DEFERRED_KEYS`], so its length is a
    /// floor rather than a total. Reported, never hidden.
    deferred_capped: bool,
    /// The state the last event carried, so staleness transitions that happen
    /// between folds are still emitted by the next getter-driven check.
    last_emitted: Option<CoordDrainState>,
    /// Whether any read (or failed read) has been folded since boot.
    folded_once: bool,
    /// The dispatch role the last SUCCESSFUL read served. `None` when it served
    /// none (or one this build does not know). Kept across failed reads.
    dispatch_role: Option<DispatchRole>,
}

struct Global {
    inner: Mutex<Inner>,
    tx: tokio::sync::watch::Sender<CoordDrainState>,
    boot_read_done: tokio::sync::Notify,
    boot_read_finished: std::sync::atomic::AtomicBool,
}

fn global() -> &'static Global {
    static GLOBAL: OnceLock<Global> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        let now = Utc::now();
        let tracker = DrainTracker::new(now);
        let (tx, _rx) = tokio::sync::watch::channel(tracker.state().clone());
        Global {
            inner: Mutex::new(Inner {
                tracker,
                last_fold: Instant::now(),
                last_fold_at: now,
                deferred: BTreeSet::new(),
                deferred_capped: false,
                last_emitted: None,
                folded_once: false,
                dispatch_role: None,
            }),
            tx,
            boot_read_done: tokio::sync::Notify::new(),
            boot_read_finished: std::sync::atomic::AtomicBool::new(false),
        }
    })
}

fn lock_inner() -> std::sync::MutexGuard<'static, Inner> {
    match global().inner.lock() {
        Ok(g) => g,
        // The contents are plain values that stay valid across a panic.
        Err(e) => e.into_inner(),
    }
}

fn heartbeat_interval() -> Duration {
    let secs: u64 = std::env::var("COORD_HEARTBEAT_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .max(1);
    Duration::from_secs(secs)
}

/// How long without a fold before the cached state reads `Unknown`: three
/// heartbeat intervals plus one read timeout of slack.
fn stale_after() -> Duration {
    heartbeat_interval() * MISSED_HEARTBEATS_TO_UNKNOWN + ME_DRAIN_TIMEOUT
}

fn current_locked(inner: &Inner) -> CoordDrainState {
    effective_state(
        inner.tracker.state(),
        inner.last_fold,
        inner.last_fold_at,
        stale_after(),
        Instant::now(),
        Utc::now(),
    )
}

/// Whether autonomous spawns run under `state` and `role` together.
fn autonomous_allowed(state: &CoordDrainState, role: Option<DispatchRole>) -> bool {
    state.allows_autonomous_spawns() && !role.is_some_and(DispatchRole::closes_agent_lane)
}

/// Whether autonomous spawns run right now: the drain AND the dispatch role.
/// The edge detectors that replay deferred work use this rather than
/// [`CoordDrainState::allows_autonomous_spawns`], which sees the drain alone.
pub fn autonomous_spawns_allowed_now() -> bool {
    let (state, role) = current_with_role();
    autonomous_allowed(&state, role)
}

/// The dispatch role the last successful coord read served. Cheap and
/// synchronous.
pub fn current_dispatch_role() -> Option<DispatchRole> {
    lock_inner().dispatch_role
}

/// The current state. Cheap and synchronous: one mutex, no I/O.
pub fn current() -> CoordDrainState {
    current_with_role().0
}

/// [`current`] and the cached dispatch role, read under ONE lock. A live gate
/// must use this rather than `current()` followed by
/// [`current_dispatch_role`]: a fold landing between two separate reads could
/// pair a state from before it with a role from after it, and allow a spawn
/// that neither reading allows.
pub fn current_with_role() -> (CoordDrainState, Option<DispatchRole>) {
    let (state, role, emit) = {
        let mut inner = lock_inner();
        let state = current_locked(&inner);
        let role = inner.dispatch_role;
        let emit = inner.last_emitted.as_ref() != Some(&state) && inner.last_emitted.is_some();
        if emit {
            inner.last_emitted = Some(state.clone());
            if autonomous_allowed(&state, inner.dispatch_role) {
                inner.deferred.clear();
                inner.deferred_capped = false;
            }
        }
        (state, role, emit)
    };
    if emit {
        // A staleness flip observed by a reader, not by a fold.
        global().tx.send_replace(state.clone());
        emit_snapshot();
    }
    (state, role)
}

/// Everything the UI and `/restart-readiness` render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordDrainSnapshot {
    /// `clear` | `drained` | `unknown` | `not_enrolled`.
    pub state: &'static str,
    /// Whether autonomous spawns run right now: the drain AND the dispatch role
    /// both allow them.
    pub autonomous_spawns_allowed: bool,
    /// The dispatch role the last successful read served (`workhorse` |
    /// `bench` | `ci_node`); `None` when coord served none.
    pub dispatch_role: Option<&'static str>,
    /// `true` when that role closes the agent lane, which defers every
    /// autonomous spawn whatever `state` says.
    pub agent_lane_closed_by_role: bool,
    pub until: Option<DateTime<Utc>>,
    pub reason: Option<String>,
    /// For `unknown`: since when.
    pub since: Option<DateTime<Utc>>,
    /// For `unknown`: why; for `not_enrolled`: what is missing.
    pub cause: Option<String>,
    /// Distinct autonomous work items deferred since the drain began.
    pub deferred_count: usize,
    /// `true` once the [`MAX_DEFERRED_KEYS`] cap was hit, which makes
    /// `deferred_count` a FLOOR rather than a total. `false` in every ordinary
    /// drain.
    pub deferred_capped: bool,
    /// The same count split by `SpawnOrigin` wire value.
    pub deferred_by_origin: BTreeMap<String, usize>,
    /// When the state was last folded from a coord read (or a failed one).
    pub last_read_at: DateTime<Utc>,
    /// `true` while the boot read has not finished AND nothing has been folded
    /// yet — the `unknown` it reports is "not read yet", which the banner does
    /// not flash at boot.
    pub boot_read_pending: bool,
}

fn snapshot_of(state: &CoordDrainState, inner: &Inner) -> CoordDrainSnapshot {
    let mut by_origin: BTreeMap<String, usize> = BTreeMap::new();
    for (origin, _) in &inner.deferred {
        *by_origin.entry(origin.as_wire().to_string()).or_default() += 1;
    }
    let (until, reason, since, cause) = match state {
        CoordDrainState::Clear => (None, None, None, None),
        CoordDrainState::Drained { until, reason } => (*until, reason.clone(), None, None),
        CoordDrainState::Unknown { since, cause } => {
            (None, None, Some(*since), Some(cause.clone()))
        }
        CoordDrainState::NotEnrolled { why } => (None, None, None, Some((*why).to_string())),
    };
    CoordDrainSnapshot {
        state: state.label(),
        autonomous_spawns_allowed: autonomous_allowed(state, inner.dispatch_role),
        dispatch_role: inner.dispatch_role.map(DispatchRole::as_wire),
        agent_lane_closed_by_role: inner
            .dispatch_role
            .is_some_and(DispatchRole::closes_agent_lane),
        until,
        reason,
        since,
        cause,
        deferred_count: inner.deferred.len(),
        deferred_capped: inner.deferred_capped,
        deferred_by_origin: by_origin,
        last_read_at: inner.last_fold_at,
        boot_read_pending: !inner.folded_once
            && !global()
                .boot_read_finished
                .load(std::sync::atomic::Ordering::SeqCst),
    }
}

/// A snapshot of `state` with nothing deferred and a fixed read time — for
/// tests of the surfaces that embed a snapshot (`/restart-readiness`).
#[cfg(test)]
pub fn snapshot_fixture(state: &CoordDrainState) -> CoordDrainSnapshot {
    let inner = Inner {
        tracker: DrainTracker::new(DateTime::<Utc>::UNIX_EPOCH),
        last_fold: Instant::now(),
        last_fold_at: DateTime::<Utc>::UNIX_EPOCH,
        deferred: BTreeSet::new(),
        deferred_capped: false,
        last_emitted: None,
        folded_once: true,
        dispatch_role: None,
    };
    snapshot_of(state, &inner)
}

/// Put the process-global cache back to its boot state: no read folded, no
/// role, `Unknown` "not read yet". For a test that drives the REAL fold path
/// (`note_heartbeat`) and must not leave its reading behind for the rest of
/// the test binary.
#[cfg(test)]
pub(crate) fn reset_global_for_test() {
    let now = Utc::now();
    let mut inner = lock_inner();
    *inner = Inner {
        tracker: DrainTracker::new(now),
        last_fold: Instant::now(),
        last_fold_at: now,
        deferred: BTreeSet::new(),
        deferred_capped: false,
        last_emitted: None,
        folded_once: false,
        dispatch_role: None,
    };
    let state = current_locked(&inner);
    drop(inner);
    // A subscriber must not keep reading the test's state after the reset.
    global().tx.send_replace(state);
}

/// The current snapshot.
pub fn snapshot() -> CoordDrainSnapshot {
    let state = current();
    let inner = lock_inner();
    snapshot_of(&state, &inner)
}

/// Subscribe to state changes. The value is the state as of the last change.
pub fn subscribe() -> tokio::sync::watch::Receiver<CoordDrainState> {
    global().tx.subscribe()
}

fn emit_snapshot() {
    let Some(handle) = crate::tauri_app_handle::current() else {
        return;
    };
    use tauri::Emitter;
    if let Err(e) = handle.emit(COORD_DRAIN_STATE_EVENT, snapshot()) {
        debug!("coord_drain_state: emit {COORD_DRAIN_STATE_EVENT} failed: {e}");
    }
}

/// What a fold does to the cached dispatch role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleUpdate {
    /// A failed read: keep the role the last successful read served.
    Keep,
    /// A successful read: replace the role with what it served (`None` when it
    /// served none).
    Set(Option<DispatchRole>),
}

/// PURE: the role update one read implies. Only a read that SERVED the key
/// replaces the cached role. A missing key is coord's failed role read (see
/// [`ServedRole`]) and keeps it, so a `ci_node` box whose role read fails once
/// stays closed rather than reopening every autonomous door until the next
/// good read. A read whose drain object was absent or did not parse, or that
/// coord answered `unreadable`, is not a successful read either and keeps it
/// too; the drain itself folds to `Unknown` and defers either way.
fn role_update_for(obs: &DrainObservation, role: ServedRole) -> RoleUpdate {
    match (obs, role) {
        (DrainObservation::Absent { .. } | DrainObservation::Unreadable, _) => RoleUpdate::Keep,
        (_, ServedRole::NotServed) => RoleUpdate::Keep,
        (_, ServedRole::Served(r)) => RoleUpdate::Set(r),
    }
}

/// PURE: the role after `update`, and whether it changed.
fn apply_role_update(
    current: Option<DispatchRole>,
    update: RoleUpdate,
) -> (Option<DispatchRole>, bool) {
    match update {
        RoleUpdate::Set(next) if next != current => (next, true),
        _ => (current, false),
    }
}

/// Fold under the lock, then publish and emit outside it.
fn fold(f: impl FnOnce(&mut DrainTracker, DateTime<Utc>) -> Transition) -> Transition {
    fold_with_role(RoleUpdate::Keep, f)
}

/// [`fold`], also applying a dispatch-role update. A role change is a change:
/// it publishes and emits like a state change, so a waiter re-checks its gate
/// and the frontend re-runs a deferred restore.
fn fold_with_role(
    role: RoleUpdate,
    f: impl FnOnce(&mut DrainTracker, DateTime<Utc>) -> Transition,
) -> Transition {
    let now = Utc::now();
    let (transition, state, role_changed, state_changed) = {
        let mut inner = lock_inner();
        let before = current_locked(&inner);
        let transition = f(&mut inner.tracker, now);
        inner.folded_once = true;
        inner.last_fold = Instant::now();
        inner.last_fold_at = now;
        let (next_role, role_changed) = apply_role_update(inner.dispatch_role, role);
        inner.dispatch_role = next_role;
        let after = current_locked(&inner);
        let state_changed = transition.changed || before != after;
        let changed = state_changed || role_changed;
        if autonomous_allowed(&after, inner.dispatch_role) {
            inner.deferred.clear();
            inner.deferred_capped = false;
        }
        if changed || inner.last_emitted.is_none() {
            inner.last_emitted = Some(after.clone());
        }
        (
            Transition {
                changed,
                entered_drained: transition.entered_drained,
            },
            after,
            role_changed.then_some(next_role),
            state_changed,
        )
    };
    if let Some(role) = role_changed {
        log_role(role);
    }
    if transition.changed {
        // A role-only change still publishes and emits (waiters re-check),
        // but the drain line is logged only when the drain itself moved.
        if state_changed {
            log_transition(&state);
        }
        global().tx.send_replace(state);
        emit_snapshot();
    }
    transition
}

fn log_transition(state: &CoordDrainState) {
    match state {
        CoordDrainState::Clear => info!("coord_drain_state: clear — autonomous spawns run"),
        CoordDrainState::Drained { until, reason } => warn!(
            until = ?until,
            reason = reason.as_deref().unwrap_or("-"),
            "coord_drain_state: DRAINED by coord — autonomous spawns are deferred"
        ),
        CoordDrainState::Unknown { since, cause } => warn!(
            since = %since,
            "coord_drain_state: UNKNOWN ({cause}) — autonomous spawns paused (fail-closed)"
        ),
        CoordDrainState::NotEnrolled { why } => {
            info!("coord_drain_state: not a coord device ({why}) — no device drain applies")
        }
    }
}

fn log_role(role: Option<DispatchRole>) {
    match role {
        Some(r) if r.closes_agent_lane() => warn!(
            dispatch_role = r.as_wire(),
            "coord_drain_state: dispatch role closes the agent lane; autonomous spawns are deferred"
        ),
        Some(r) => info!(
            dispatch_role = r.as_wire(),
            "coord_drain_state: dispatch role opens the agent lane"
        ),
        None => info!("coord_drain_state: coord served no dispatch role; the drain alone decides"),
    }
}

/// Record that the autonomous `origin` work `key` was deferred by the drain.
/// Idempotent per `(origin, key)`; cleared when autonomous spawns resume.
///
/// BOUNDED at [`MAX_DEFERRED_KEYS`] distinct keys. Some doors key their
/// deferral on a caller-supplied string (see [`bounded_work_key`]), so an
/// unbounded set is a remote-influenced allocation during a long drain. At the
/// cap nothing further is stored and `deferred_capped` goes true, which makes
/// `deferred_count` a FLOOR — the banner reads "512+ deferred work items"
/// rather than a total it cannot support.
///
/// The cap deliberately records a FLAG and not a count of what it refused. A
/// counter would have to be incremented on every refused call, and a call at
/// the cap is exactly the repeated one a bounded set cannot deduplicate — so it
/// would emit a Tauri snapshot per call for the drain's duration (an emit storm
/// on the remote-reachable doors this cap exists to defend), and the number
/// would count CALLS beside a distinct-KEY count, which is not comparable to
/// the figure it qualifies.
pub fn record_deferral(origin: SpawnOrigin, key: &str) {
    let emit = {
        let mut inner = lock_inner();
        if autonomous_allowed(&current_locked(&inner), inner.dispatch_role) {
            false
        } else if inner.deferred.len() >= MAX_DEFERRED_KEYS {
            // At the cap. Flip the flag ONCE (that transition is worth an
            // emit); every later refused call is silent.
            let first = !inner.deferred_capped;
            inner.deferred_capped = true;
            first
        } else {
            inner.deferred.insert((origin, key.to_string()))
        }
    };
    if emit {
        emit_snapshot();
    }
}

/// Resolve once autonomous spawns of `origin` may run. Wakes on every state
/// change and re-checks every heartbeat interval (staleness is time-based).
pub async fn wait_until_allowed(origin: SpawnOrigin) {
    let mut rx = subscribe();
    loop {
        if drain_gate(origin).allows() {
            return;
        }
        match tokio::time::timeout(heartbeat_interval(), rx.changed()).await {
            Ok(Ok(())) | Err(_) => {}
            // The sender lives in a static and is never dropped; a closed
            // channel is unreachable, but must not become a busy loop.
            Ok(Err(_)) => tokio::time::sleep(heartbeat_interval()).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Staggered release (the thundering-herd bound)
// ---------------------------------------------------------------------------

/// Minimum spacing between two HELD work items starting after a drain lifts.
///
/// The wave is paced, not capped, so the LAST of N held tasks waits about
/// `N * RELEASE_SPACING`: a drain that accumulated 1000 of them releases over
/// roughly 25 minutes. That is the intended behaviour and not a hang — every
/// task is queued, none is dropped, and each one logs its own release.
pub const RELEASE_SPACING: Duration = Duration::from_millis(1_500);

/// Upper bound on the random jitter added to each release slot, so a release
/// wave does not land on a fixed grid either.
pub const RELEASE_JITTER: Duration = Duration::from_millis(750);

/// The next release slot, as a monotonic instant. `None` until the first
/// staggered release; reset is unnecessary because a slot in the past is
/// simply overtaken by `now`.
fn release_slot() -> &'static Mutex<Option<tokio::time::Instant>> {
    static SLOT: OnceLock<Mutex<Option<tokio::time::Instant>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// PURE: the slot a release claims, given the previously claimed one.
///
/// `now` when nothing is queued ahead; otherwise `spacing + jitter` past the
/// last claimed slot. Monotone by construction: a caller that claims LATER
/// never gets an earlier slot.
///
/// That orders releases by CLAIM order, which is mutex-acquisition order after
/// `tx.send_replace` wakes every waiter at once — scheduler order, not the
/// order the tasks arrived at the hold, which can be hours apart. Pacing is the
/// property this buys; fairness is not, and nothing downstream needs it.
pub fn next_release_slot(
    now: tokio::time::Instant,
    last: Option<tokio::time::Instant>,
    spacing: Duration,
    jitter: Duration,
) -> tokio::time::Instant {
    match last {
        Some(last) if last >= now => last + spacing + jitter,
        _ => now,
    }
}

/// Claim this task's release slot and wait for it.
///
/// **Why this exists (independent review S5).** `held_until_allowed` is applied
/// INSIDE already-spawned workflow tasks, so over a long drain N of them pile
/// up — each holding a `WorkflowDropGuard` and a task-run row that reads
/// *running* with no progress. `tx.send_replace` then wakes ALL N in the same
/// instant. The machine-load backstop (`agent_runtime::admit_launch` /
/// `evaluate_load_guard`) sits on the COORD-LAUNCH path and does not see this
/// one, so nothing downstream bounds the wave.
///
/// **What bounds it here, exactly:** the release RATE, to one task per
/// [`RELEASE_SPACING`] plus up to [`RELEASE_JITTER`] — a single global slot
/// cursor that each releasing task advances before sleeping until its own slot.
/// N tasks therefore start spread over roughly `N * RELEASE_SPACING`, in
/// arrival order, instead of simultaneously. It is a PACER, not a cap: nothing
/// is dropped and no permit is held across the work itself, so a long-running
/// workflow never blocks the next release.
async fn stagger_release() {
    let jitter = {
        use rand::Rng;
        let millis = u64::try_from(RELEASE_JITTER.as_millis()).unwrap_or(0);
        Duration::from_millis(rand::rng().random_range(0..=millis))
    };
    let slot = {
        let mut guard = match release_slot().lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let slot = next_release_slot(tokio::time::Instant::now(), *guard, RELEASE_SPACING, jitter);
        *guard = Some(slot);
        slot
    };
    tokio::time::sleep_until(slot).await;
}

/// Run `fut` once autonomous spawns of `origin` may run: a HOLD, never a
/// refusal. For fire-once autonomous launches that have nowhere to leave the
/// work pending (workflow triggers): while the device is drained or its drain
/// state is unknown the future waits, counted on the banner under `work_key`,
/// and starts the moment the drain lifts. Nothing inside `fut` runs — no
/// `claude` process exists — until then.
///
/// A hold that actually waited is released through [`stagger_release`], which
/// paces the wave — read its doc comment for what bounds it.
pub async fn held_until_allowed<F: std::future::Future>(
    origin: SpawnOrigin,
    work_key: String,
    fut: F,
) -> F::Output {
    if let DrainGate::Defer { reason, .. } = drain_gate_for_work(origin, &work_key) {
        info!("coord_drain_state: holding {work_key} — {reason}");
        wait_until_allowed(origin).await;
        stagger_release().await;
        info!("coord_drain_state: releasing {work_key} — autonomous spawns allowed again");
    }
    fut.await
}

/// The HTTP refusal body for a drain deferral: the reason, with the
/// `device_drained` / `drain_unreadable` code the deferring state implies.
pub fn api_refusal(reason: &str, class: DeferClass) -> crate::mcp::types::ApiResponse<()> {
    let mut body = crate::mcp::types::api_error(reason.to_string());
    body.code = Some(class.code().to_string());
    body
}

// ---------------------------------------------------------------------------
// Coord reads
// ---------------------------------------------------------------------------

/// What the heartbeat tick learned, handed to [`note_heartbeat`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// Coord accepted the register POST; its body's `drain` object and the
    /// `dispatch_role` served inside it.
    Registered {
        drain: DrainObservation,
        dispatch_role: ServedRole,
    },
    /// No register POST was sent although this runner IS a coord device: a
    /// secondary instance (the primary owns the machine payload), or a device
    /// with no tenant binding yet. It still reads `me/drain` on its device JWT;
    /// a failed read is a miss that trends to `Unknown`, never `NotEnrolled`.
    NotSent { why: &'static str },
    /// No heartbeat is possible because this runner is not a coord device: no
    /// `machine.json` or no coord URL.
    NotEnrolled { why: &'static str },
}

/// What the local files say about this runner's coord enrollment.
///
/// THREE answers, not two, and the third is the whole point. `NotEnrolled`
/// ALLOWS autonomous spawns — it is the "this box has no coord, nothing can
/// drain it" arm — so anything that cannot be established must NOT land there.
/// Both local reads return `Option` and swallow the difference between *absent*
/// and *unreadable*: `machine.json` under a momentary lock (a concurrent
/// `qontinui_profile device init`, an AV scan, a half-written file) read as
/// "missing", and so did an unreadable `settings.json`, whose own
/// `UnknownTierProdDefault` arm exists to say "we could not read your tier".
/// Either one made [`fold`] see `allows_autonomous_spawns()`, clear `deferred`,
/// and wake every held spawn at once on a device coord still holds DRAINED.
///
/// So an indeterminate local read is `Indeterminate`, which the callers fold as
/// a MISS — trending to `Unknown`, which defers. Absent stays `NotEnrolled`.
/// The network direction already had this right: every non-2xx and transport
/// error is an `Err` from [`read_me_drain`] and folds as a miss.
enum Enrollment {
    /// A coord device, with this base.
    Enrolled(String),
    /// Positively NOT a coord device: the file is genuinely absent, or names no
    /// device, or no coord is configured. Autonomous spawns run.
    NotEnrolled(&'static str),
    /// Could not be established. Fails CLOSED — folded as a miss, not as
    /// `NotEnrolled`.
    Indeterminate(String),
}

/// Why this runner is not a coord device, the base if it is, or that the local
/// files could not answer. See [`Enrollment`].
fn enrollment() -> Enrollment {
    use qontinui_runner_lib::ambient::MachineJsonError;

    let machine = match qontinui_runner_lib::ambient::try_read_machine_json() {
        Ok(machine) => machine,
        // The one arm that is genuinely "no device file": ENOENT.
        Err(e) if e.is_missing() => {
            return Enrollment::NotEnrolled("~/.qontinui/machine.json is missing")
        }
        // No home dir at all: there is no device identity to have, and no
        // amount of retrying produces one.
        Err(MachineJsonError::NoHomeDir) => {
            return Enrollment::NotEnrolled("no home directory, so there is no machine.json")
        }
        // A permission error, a mid-write truncation, invalid JSON: the file
        // may well name a drained device. Fail closed.
        Err(e) => {
            return Enrollment::Indeterminate(format!(
                "~/.qontinui/machine.json could not be read: {e}"
            ))
        }
    };
    let Some(device_id) = machine.device_id.as_deref() else {
        return Enrollment::NotEnrolled("machine.json names no device_id");
    };
    if uuid::Uuid::parse_str(device_id).is_err() {
        // STATED but garbage — a device that is probably enrolled behind a
        // corrupt file, which is not the same claim as "not a coord device".
        return Enrollment::Indeterminate(
            "machine.json device_id is stated but is not a UUID".to_string(),
        );
    }
    // A tenant binding is NOT required: a device with `machine.json` and a coord
    // URL is a coord device coord can drain, and `me/drain` answers on the
    // device JWT alone. Treating an unpaired device as not-enrolled would let
    // it spawn autonomously while coord holds it drained.
    if let Some(base) = qontinui_runner_lib::profiles::connected_coord_base() {
        return Enrollment::Enrolled(base);
    }
    // `connected_coord_base` said "isolated". That is only NotEnrolled when the
    // tier was actually READ; the `UnknownTierProdDefault` arm means
    // settings.json was UNREADABLE and prod was a guess, which is indeterminate.
    match qontinui_runner_lib::profiles::coord_base_policy().1 {
        qontinui_runner_lib::profiles::CoordBaseSource::UnknownTierProdDefault => {
            Enrollment::Indeterminate(
                "settings.json could not be read, so the coord base is unknown".to_string(),
            )
        }
        _ => Enrollment::NotEnrolled("the active profile has no coord_url"),
    }
}

/// `GET {base}/coord/devices/me/drain` → an observation, or the failure cause.
async fn read_me_drain(base: &str) -> Result<(DrainObservation, ServedRole), String> {
    let url = format!("{}/coord/devices/me/drain", base.trim_end_matches('/'));
    // A per-read client, like the heartbeat's: the caller may run on the
    // heartbeat's dedicated current-thread runtime, and a pooled connection
    // must not outlive the runtime that opened it.
    let client = reqwest::Client::builder()
        .timeout(ME_DRAIN_TIMEOUT)
        .build()
        .map_err(|e| format!("reqwest builder: {e}"))?;
    // coord-tenant-scope(device): the route answers for the device JWT's own device; there is no tenant in the request.
    let resp = crate::auth::attach_device_auth(client.get(&url))
        .send()
        .await
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("GET {url} returned {status}: {excerpt}"));
    }
    Ok((parse_me_drain_response(&body), parse_me_drain_role(&body)))
}

/// Fold one heartbeat tick's result. Reads `me/drain` when the tick shows a
/// transition into `drained` (for `reason`), and on a secondary instance.
pub async fn note_heartbeat(outcome: HeartbeatOutcome) {
    match outcome {
        HeartbeatOutcome::Registered {
            drain,
            dispatch_role,
        } => {
            let update = role_update_for(&drain, dispatch_role);
            let t = fold_with_role(update, |tr, now| tr.observe(drain, now));
            if t.entered_drained {
                refresh_reason().await;
            }
        }
        HeartbeatOutcome::NotSent { why } => match enrollment() {
            Enrollment::Enrolled(base) => match read_me_drain(&base).await {
                Ok((obs, role)) => {
                    let update = role_update_for(&obs, role);
                    fold_with_role(update, |tr, now| tr.observe(obs, now));
                }
                Err(e) => {
                    fold(|tr, now| tr.miss(&format!("{why}; me/drain: {e}"), now));
                }
            },
            Enrollment::NotEnrolled(not) => {
                fold_with_role(RoleUpdate::Set(None), |tr, now| {
                    tr.observe(DrainObservation::NotEnrolled { why: not }, now)
                });
            }
            Enrollment::Indeterminate(cause) => {
                fold(|tr, now| tr.miss(&format!("{why}; {cause}"), now));
            }
        },
        HeartbeatOutcome::NotEnrolled { why } => {
            fold_with_role(RoleUpdate::Set(None), |tr, now| {
                tr.observe(DrainObservation::NotEnrolled { why }, now)
            });
        }
    }
}

/// Fold one FAILED heartbeat tick.
pub fn note_heartbeat_failure(cause: &str) {
    fold(|tr, now| tr.miss(cause, now));
}

/// On a transition into `drained`: read `me/drain` for the operator's reason.
/// A failed read leaves the drained state as the heartbeat reported it.
async fn refresh_reason() {
    let Enrollment::Enrolled(base) = enrollment() else {
        return;
    };
    match read_me_drain(&base).await {
        Ok((obs @ (DrainObservation::Drained { .. } | DrainObservation::Clear), role)) => {
            let update = role_update_for(&obs, role);
            fold_with_role(update, |tr, now| tr.observe(obs, now));
        }
        Ok(other) => debug!("coord_drain_state: me/drain reason read answered {other:?}; kept the heartbeat's state"),
        Err(e) => debug!("coord_drain_state: me/drain reason read failed ({e}); kept the heartbeat's state"),
    }
}

/// The boot read: `GET /coord/devices/me/drain` once, before any restore or
/// resume. Idempotent — the first call reads, later calls return at once.
pub async fn boot_read() {
    let g = global();
    if g.boot_read_finished
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    match enrollment() {
        Enrollment::Enrolled(base) => match read_me_drain(&base).await {
            Ok((obs, role)) => {
                let update = role_update_for(&obs, role);
                fold_with_role(update, |tr, now| tr.observe(obs, now));
            }
            Err(e) => {
                fold(|tr, now| tr.miss(&format!("boot read: {e}"), now));
            }
        },
        Enrollment::Indeterminate(cause) => {
            fold(|tr, now| tr.miss(&format!("boot read: {cause}"), now));
        }
        Enrollment::NotEnrolled(why) => {
            fold_with_role(RoleUpdate::Set(None), |tr, now| {
                tr.observe(DrainObservation::NotEnrolled { why }, now)
            });
        }
    }
    g.boot_read_finished
        .store(true, std::sync::atomic::Ordering::SeqCst);
    g.boot_read_done.notify_waiters();
    info!(
        state = current().label(),
        "coord_drain_state: boot read complete"
    );
}

/// Wait (bounded) for [`boot_read`] to have completed, so a boot resume or
/// restore decides against a real read rather than the not-yet-read state.
/// Returns on timeout too — the gate then sees `Unknown` and defers.
pub async fn await_boot_read(timeout: Duration) {
    let g = global();
    // Register interest BEFORE reading the flag: `notify_waiters` wakes only a
    // future that is already enabled, so a boot read finishing between the
    // flag check and the first poll would otherwise be missed and cost the
    // whole timeout.
    let notified = g.boot_read_done.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    if g.boot_read_finished
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let _ = tokio::time::timeout(timeout, notified).await;
}

/// `coord_drain_state_get` — the banner's getter.
#[tauri::command]
pub fn coord_drain_state_get() -> CoordDrainSnapshot {
    snapshot()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-14T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn spawn_origin_wire_vocabulary_is_exactly_the_contract() {
        let wire: Vec<&str> = SpawnOrigin::ALL.iter().map(|o| o.as_wire()).collect();
        assert_eq!(
            wire,
            vec![
                "gate_continuation",
                "unit_continuation",
                "coord_dispatch",
                "looping_agent",
                "steward",
                "respawn",
                "scheduler",
                "orchestration",
                "boot_resume",
                "operator_terminal",
                "operator_chat",
                "unknown",
            ]
        );
        for o in SpawnOrigin::ALL {
            assert_eq!(serde_json::to_value(o).unwrap(), o.as_wire());
        }
    }

    #[test]
    fn only_the_operator_origins_are_not_autonomous() {
        let operator: Vec<_> = SpawnOrigin::ALL
            .into_iter()
            .filter(|o| !o.is_autonomous())
            .collect();
        assert_eq!(
            operator,
            vec![SpawnOrigin::OperatorTerminal, SpawnOrigin::OperatorChat]
        );
        assert!(SpawnOrigin::Unknown.is_autonomous(), "unknown fails closed");
    }

    #[test]
    fn gate_defers_autonomous_while_drained_or_unknown_and_never_the_operator() {
        let drained = CoordDrainState::Drained {
            until: None,
            reason: Some("rebuild".into()),
        };
        let unknown = CoordDrainState::Unknown {
            since: t0(),
            cause: "x".into(),
        };
        for state in [&drained, &unknown] {
            for o in SpawnOrigin::ALL {
                let g = gate_for(state, o);
                assert_eq!(g.allows(), !o.is_autonomous(), "{state:?} {o:?}");
            }
        }
        for state in [
            CoordDrainState::Clear,
            CoordDrainState::NotEnrolled { why: "no coord" },
        ] {
            for o in SpawnOrigin::ALL {
                assert_eq!(gate_for(&state, o), DrainGate::Allow);
            }
        }
        match gate_for(&drained, SpawnOrigin::Steward) {
            DrainGate::Defer { reason, class } => {
                assert_eq!(class, DeferClass::Drained);
                assert!(
                    reason.contains("rebuild") && reason.contains("steward"),
                    "{reason}"
                )
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn register_response_parses_each_state_and_absence_is_not_clear() {
        assert_eq!(
            parse_register_response(r#"{"tenant_ids":[],"drain":{"state":"clear"}}"#),
            DrainObservation::Clear
        );
        assert_eq!(
            parse_register_response(
                r#"{"drain":{"state":"drained","until":"2026-09-14T12:00:00Z"}}"#
            ),
            DrainObservation::Drained {
                until: Some(t0() + chrono::Duration::hours(2)),
                reason: None
            }
        );
        assert_eq!(
            parse_register_response(r#"{"drain":{"state":"unreadable"}}"#),
            DrainObservation::Unreadable
        );
        for body in [
            r#"{"tenant_ids":[]}"#,
            r#"{"drain":null}"#,
            r#"{"drain":{"state":"paused"}}"#,
            r#"{"drain":{"until":"x"}}"#,
            "not json",
        ] {
            assert!(
                matches!(
                    parse_register_response(body),
                    DrainObservation::Absent { .. }
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn me_drain_response_carries_reason() {
        let body = r#"{"device_id":"eb2155ed-4152-4a91-be82-5d4346f717fc","as_of":"2026-09-14T10:00:00Z","state":"drained","until":"2026-09-14T12:00:00Z","reason":"rebuild runner"}"#;
        assert_eq!(
            parse_me_drain_response(body),
            DrainObservation::Drained {
                until: Some(t0() + chrono::Duration::hours(2)),
                reason: Some("rebuild runner".into())
            }
        );
    }

    #[test]
    fn tracker_starts_unknown_and_folds_observations() {
        let mut tr = DrainTracker::new(t0());
        assert!(matches!(tr.state(), CoordDrainState::Unknown { .. }));
        let t = tr.observe(DrainObservation::Clear, t0());
        assert!(t.changed && !t.entered_drained);
        assert_eq!(tr.state(), &CoordDrainState::Clear);

        let until = Some(t0() + chrono::Duration::hours(1));
        let t = tr.observe(
            DrainObservation::Drained {
                until,
                reason: None,
            },
            t0(),
        );
        assert!(t.changed && t.entered_drained);
        // me/drain supplies the reason; a later heartbeat without one keeps it.
        let t = tr.observe(
            DrainObservation::Drained {
                until,
                reason: Some("rebuild".into()),
            },
            t0(),
        );
        assert!(t.changed && !t.entered_drained);
        let t = tr.observe(
            DrainObservation::Drained {
                until,
                reason: None,
            },
            t0(),
        );
        assert!(!t.changed);
        assert_eq!(
            tr.state(),
            &CoordDrainState::Drained {
                until,
                reason: Some("rebuild".into())
            }
        );
        // A re-drain with a new deadline asks for the reason again.
        let later = Some(t0() + chrono::Duration::hours(3));
        let t = tr.observe(
            DrainObservation::Drained {
                until: later,
                reason: None,
            },
            t0(),
        );
        assert!(t.entered_drained);
        assert_eq!(
            tr.state(),
            &CoordDrainState::Drained {
                until: later,
                reason: None
            }
        );
    }

    #[test]
    fn an_expired_until_becomes_clear_on_the_next_read() {
        let mut tr = DrainTracker::new(t0());
        let until = Some(t0() + chrono::Duration::minutes(5));
        tr.observe(
            DrainObservation::Drained {
                until,
                reason: None,
            },
            t0(),
        );
        tr.observe(
            DrainObservation::Drained {
                until,
                reason: None,
            },
            t0() + chrono::Duration::minutes(6),
        );
        assert_eq!(tr.state(), &CoordDrainState::Clear);
    }

    #[test]
    fn unreadable_and_absent_are_unknown_never_clear() {
        let mut tr = DrainTracker::new(t0());
        tr.observe(DrainObservation::Clear, t0());
        tr.observe(DrainObservation::Unreadable, t0());
        assert!(matches!(tr.state(), CoordDrainState::Unknown { .. }));
        tr.observe(DrainObservation::Clear, t0());
        tr.observe(
            DrainObservation::Absent {
                detail: "no drain".into(),
            },
            t0(),
        );
        assert!(
            matches!(tr.state(), CoordDrainState::Unknown { cause, .. } if cause == "no drain")
        );
    }

    #[test]
    fn three_missed_heartbeats_become_unknown_and_a_read_resets_the_count() {
        let mut tr = DrainTracker::new(t0());
        tr.observe(DrainObservation::Clear, t0());
        assert!(!tr.miss("timeout", t0()).changed);
        assert!(!tr.miss("timeout", t0()).changed);
        assert_eq!(tr.state(), &CoordDrainState::Clear);
        tr.observe(DrainObservation::Clear, t0());
        tr.miss("timeout", t0());
        tr.miss("timeout", t0());
        assert_eq!(
            tr.state(),
            &CoordDrainState::Clear,
            "the read reset the streak"
        );
        let t = tr.miss("timeout", t0());
        assert!(t.changed);
        assert!(
            matches!(tr.state(), CoordDrainState::Unknown { cause, .. } if cause.contains("3 consecutive"))
        );
    }

    #[test]
    fn a_miss_while_drained_keeps_the_drain_until_the_third() {
        let mut tr = DrainTracker::new(t0());
        let d = DrainObservation::Drained {
            until: None,
            reason: None,
        };
        tr.observe(d, t0());
        tr.miss("x", t0());
        tr.miss("x", t0());
        assert!(matches!(tr.state(), CoordDrainState::Drained { .. }));
        tr.miss("x", t0());
        assert!(matches!(tr.state(), CoordDrainState::Unknown { .. }));
    }

    #[test]
    fn not_enrolled_allows_autonomous_spawns() {
        let mut tr = DrainTracker::new(t0());
        tr.observe(DrainObservation::NotEnrolled { why: "no coord" }, t0());
        assert!(tr.state().allows_autonomous_spawns());
    }

    /// …which is exactly why an UNREADABLE local file must not reach that arm.
    /// The review found the two local reads swallowing the difference between
    /// *absent* and *unreadable*, so a momentary lock on `machine.json` (a
    /// concurrent `qontinui_profile device init`, an AV scan, a half-written
    /// file) released every held spawn on a device coord still held DRAINED.
    ///
    /// This pins the fold each arm gets, which is the part that decides it: an
    /// indeterminate read is a MISS (trending to `Unknown`, which defers), never
    /// an observation of `NotEnrolled`.
    #[test]
    fn an_indeterminate_local_read_defers_while_an_absent_one_allows() {
        // Absent: positively not a coord device — spawns run.
        let mut absent = DrainTracker::new(t0());
        absent.observe(
            DrainObservation::NotEnrolled {
                why: "~/.qontinui/machine.json is missing",
            },
            t0(),
        );
        assert!(
            absent.state().allows_autonomous_spawns(),
            "a box with no machine.json has no coord that could drain it"
        );

        // Indeterminate, folded as a miss: after the miss budget the state is
        // Unknown, which DEFERS. The drained device stays deferred.
        let mut unreadable = DrainTracker::new(t0());
        for _ in 0..3 {
            unreadable.miss(
                "~/.qontinui/machine.json could not be read: permission denied",
                t0(),
            );
        }
        assert!(
            matches!(unreadable.state(), CoordDrainState::Unknown { .. }),
            "an unreadable machine.json must trend to Unknown, got {:?}",
            unreadable.state()
        );
        assert!(
            !unreadable.state().allows_autonomous_spawns(),
            "an unreadable local file must FAIL CLOSED — it may well name a drained device"
        );
    }

    #[test]
    fn staleness_turns_a_known_state_unknown_after_three_intervals() {
        let base = Instant::now();
        let stale = Duration::from_secs(95);
        let clear = CoordDrainState::Clear;
        assert_eq!(
            effective_state(
                &clear,
                base,
                t0(),
                stale,
                base + Duration::from_secs(60),
                t0()
            ),
            clear
        );
        assert!(matches!(
            effective_state(&clear, base, t0(), stale, base + Duration::from_secs(96), t0()),
            CoordDrainState::Unknown { since, .. } if since == t0()
        ));
        let drained = CoordDrainState::Drained {
            until: None,
            reason: None,
        };
        assert!(matches!(
            effective_state(
                &drained,
                base,
                t0(),
                stale,
                base + Duration::from_secs(200),
                t0()
            ),
            CoordDrainState::Unknown { .. }
        ));
    }

    #[test]
    fn snapshot_serializes_camel_case_with_counts() {
        let inner = Inner {
            tracker: DrainTracker::new(t0()),
            last_fold: Instant::now(),
            last_fold_at: t0(),
            deferred: [
                (SpawnOrigin::Steward, "merge-train".to_string()),
                (SpawnOrigin::LoopingAgent, "a1".to_string()),
                (SpawnOrigin::LoopingAgent, "a2".to_string()),
            ]
            .into_iter()
            .collect(),
            deferred_capped: false,
            last_emitted: None,
            folded_once: true,
            dispatch_role: None,
        };
        let snap = snapshot_of(
            &CoordDrainState::Drained {
                until: None,
                reason: Some("rebuild".into()),
            },
            &inner,
        );
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["state"], "drained");
        assert_eq!(json["autonomousSpawnsAllowed"], false);
        assert_eq!(json["reason"], "rebuild");
        assert_eq!(json["deferredCount"], 3);
        assert_eq!(json["deferredByOrigin"]["looping_agent"], 2);
        assert_eq!(json["deferredByOrigin"]["steward"], 1);
    }

    #[test]
    fn a_drain_past_its_until_allows_before_any_read_folds_it() {
        let until = Some(t0() + chrono::Duration::minutes(5));
        let drained = CoordDrainState::Drained {
            until,
            reason: None,
        };
        assert!(!gate_for_at(&drained, SpawnOrigin::Steward, t0()).allows());
        assert!(gate_for_at(
            &drained,
            SpawnOrigin::Steward,
            t0() + chrono::Duration::minutes(5)
        )
        .allows());
        let base = Instant::now();
        assert_eq!(
            effective_state(
                &drained,
                base,
                t0(),
                Duration::from_secs(95),
                base,
                t0() + chrono::Duration::minutes(6)
            ),
            CoordDrainState::Clear
        );
    }

    #[test]
    fn a_deferral_names_the_class_of_the_state_that_deferred_it() {
        let unknown = CoordDrainState::Unknown {
            since: t0(),
            cause: "x".into(),
        };
        match gate_for(&unknown, SpawnOrigin::Scheduler) {
            DrainGate::Defer { class, .. } => {
                assert_eq!(class, DeferClass::Unknown);
                assert_eq!(class.code(), "drain_unreadable");
                assert_eq!(class.label(), "unknown");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(DeferClass::Drained.code(), "device_drained");
        let body = serde_json::to_value(api_refusal("r", DeferClass::Unknown)).unwrap();
        assert_eq!(body["code"], "drain_unreadable");
        assert_eq!(body["error"], "r");
    }

    #[tokio::test]
    async fn held_until_allowed_runs_the_future_at_once_when_nothing_defers() {
        // A non-autonomous origin never defers, whatever the global state is.
        let out = held_until_allowed(SpawnOrigin::OperatorChat, "k".into(), async { 7 }).await;
        assert_eq!(out, 7);
    }

    /// Review N4: a caller-supplied key component is passed through only while
    /// it is short and key-safe; anything else becomes a fixed-width digest, so
    /// the key length is bounded whatever the caller sends.
    #[test]
    fn a_work_key_is_bounded_however_long_the_callers_half_is() {
        assert_eq!(
            bounded_work_key("http_terminal", "merge-train"),
            "http_terminal:merge-train"
        );
        let long = "x".repeat(10_000);
        let key = bounded_work_key("relay_terminal", &long);
        assert!(
            key.len() <= "relay_terminal".len() + 1 + MAX_WORK_KEY_TAIL,
            "unbounded key: {} bytes",
            key.len()
        );
        // Stable per distinct value, and distinct values stay distinct.
        assert_eq!(key, bounded_work_key("relay_terminal", &long));
        assert_ne!(key, bounded_work_key("relay_terminal", &"y".repeat(10_000)));
        // A newline or a control character is digested rather than embedded in
        // a key that is logged and rendered.
        assert!(bounded_work_key("proxy_terminal", "a\nb").contains(":#"));
        assert!(bounded_work_key("proxy_terminal", "").contains(":#"));
    }

    /// Review S5: the release pacer is monotone and spaces successive claims,
    /// so N held tasks start spread out instead of all at once.
    #[test]
    fn the_release_pacer_spaces_successive_claims_and_never_moves_backwards() {
        let spacing = Duration::from_millis(1_500);
        let jitter = Duration::from_millis(100);
        let t0 = tokio::time::Instant::now();

        // Nothing queued: start now.
        assert_eq!(next_release_slot(t0, None, spacing, jitter), t0);
        // A slot already claimed for now: the next one is spacing + jitter later.
        let second = next_release_slot(t0, Some(t0), spacing, jitter);
        assert_eq!(second, t0 + spacing + jitter);
        // ...and a third stacks on the second, so ten tasks take ~10 * spacing.
        let third = next_release_slot(t0, Some(second), spacing, jitter);
        assert_eq!(third, second + spacing + jitter);
        // A stale slot (already past) does not hold anything back.
        let later = t0 + Duration::from_secs(60);
        assert_eq!(next_release_slot(later, Some(t0), spacing, jitter), later);
    }

    // -- Dispatch role ------------------------------------------------------

    #[test]
    fn dispatch_role_parses_coords_three_spellings_and_nothing_else() {
        assert_eq!(
            DispatchRole::parse("workhorse"),
            Some(DispatchRole::Workhorse)
        );
        assert_eq!(DispatchRole::parse("bench"), Some(DispatchRole::Bench));
        assert_eq!(DispatchRole::parse("ci_node"), Some(DispatchRole::CiNode));
        assert_eq!(DispatchRole::parse("CI_NODE"), None);
        assert_eq!(DispatchRole::parse("mystery"), None);
        for r in [
            DispatchRole::Workhorse,
            DispatchRole::Bench,
            DispatchRole::CiNode,
        ] {
            assert_eq!(DispatchRole::parse(r.as_wire()), Some(r));
        }
        assert!(!DispatchRole::Workhorse.closes_agent_lane());
        assert!(DispatchRole::Bench.closes_agent_lane());
        assert!(DispatchRole::CiNode.closes_agent_lane());
    }

    #[test]
    fn the_role_is_read_from_both_wires_and_absence_or_an_unknown_spelling_is_none() {
        // The me/drain answer coord serves a ci_node device with a clear drain.
        assert_eq!(
            parse_me_drain_role(r#"{"state":"clear","dispatch_role":"ci_node"}"#),
            ServedRole::Served(Some(DispatchRole::CiNode))
        );
        assert_eq!(
            parse_register_role(r#"{"drain":{"state":"clear","dispatch_role":"bench"}}"#),
            ServedRole::Served(Some(DispatchRole::Bench))
        );
        // A missing key is a role that was NOT read, never "no role".
        assert_eq!(
            parse_me_drain_role(r#"{"state":"clear"}"#),
            ServedRole::NotServed
        );
        assert_eq!(
            parse_register_role(r#"{"drain":{"state":"clear"}}"#),
            ServedRole::NotServed
        );
        assert_eq!(
            parse_register_role(r#"{"drain":{"state":"clear","dispatch_role":7}}"#),
            ServedRole::NotServed
        );
        // A served spelling this build does not know is served-but-unknown.
        assert_eq!(
            parse_register_role(r#"{"drain":{"state":"clear","dispatch_role":"mystery"}}"#),
            ServedRole::Served(None)
        );
        assert_eq!(
            parse_register_role(r#"{"dispatch_role":"ci_node"}"#),
            ServedRole::NotServed,
            "only the drain object's key counts on the register wire"
        );
        assert_eq!(parse_me_drain_role("not json"), ServedRole::NotServed);
        // The role never changes how the drain itself parses.
        assert_eq!(
            parse_me_drain_response(r#"{"state":"clear","dispatch_role":"ci_node"}"#),
            DrainObservation::Clear
        );
    }

    /// The defect this exists for: coord answers `clear` + `ci_node`, and the
    /// runner used to read only `clear` and let every autonomous path run.
    #[test]
    fn a_role_that_closes_the_agent_lane_defers_every_autonomous_origin_while_the_drain_is_clear() {
        for role in [DispatchRole::CiNode, DispatchRole::Bench] {
            for state in [
                CoordDrainState::Clear,
                CoordDrainState::NotEnrolled { why: "x" },
            ] {
                for o in SpawnOrigin::ALL {
                    let g = gate_with_role_at(&state, Some(role), o, t0());
                    if o.is_autonomous() {
                        match g {
                            DrainGate::Defer { reason, class } => {
                                assert_eq!(class, DeferClass::RoleClosed, "{role:?} {o:?}");
                                assert!(reason.contains(role.as_wire()), "{reason}");
                            }
                            DrainGate::Allow => panic!("{role:?} {state:?} allowed {o:?}"),
                        }
                    } else {
                        assert_eq!(
                            g,
                            DrainGate::Allow,
                            "{role:?} must not touch the operator's {o:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_role_reason_carries_exactly_one_promise_tail_like_the_drain_reasons() {
        match role_gate(Some(DispatchRole::CiNode), SpawnOrigin::Unknown) {
            DrainGate::Defer { reason, .. } => {
                let (head, tail) = reason.rsplit_once(" — ").expect("a \" — \" tail");
                assert!(
                    head.contains("ci_node") && head.contains("agent lane"),
                    "{head}"
                );
                assert!(tail.contains("runs only if the role changes"), "{tail}");
                assert_eq!(reason.matches(" — ").count(), 1, "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_read_whose_drain_object_did_not_parse_keeps_the_role() {
        use ServedRole::{NotServed, Served};
        let ci = Some(DispatchRole::CiNode);
        let absent = DrainObservation::Absent { detail: "x".into() };
        assert_eq!(role_update_for(&absent, NotServed), RoleUpdate::Keep);
        assert_eq!(role_update_for(&absent, Served(ci)), RoleUpdate::Keep);
        assert_eq!(
            role_update_for(&DrainObservation::Unreadable, Served(ci)),
            RoleUpdate::Keep
        );
        assert_eq!(
            role_update_for(&DrainObservation::Clear, Served(ci)),
            RoleUpdate::Set(ci)
        );
        // Served, but a spelling this build does not know: today's behaviour.
        assert_eq!(
            role_update_for(&DrainObservation::Clear, Served(None)),
            RoleUpdate::Set(None)
        );
        assert_eq!(
            role_update_for(
                &DrainObservation::Drained {
                    until: None,
                    reason: None
                },
                Served(Some(DispatchRole::Bench))
            ),
            RoleUpdate::Set(Some(DispatchRole::Bench))
        );
    }

    /// Review S1: coord omits `dispatch_role` exactly when its ROLE read
    /// failed, and still serves a readable `clear`. That must not reopen the
    /// agent lane on a `ci_node` box: the cached role is kept.
    #[test]
    fn a_readable_drain_that_omits_the_role_keeps_a_closing_role() {
        let ci = Some(DispatchRole::CiNode);
        let update = role_update_for(
            &DrainObservation::Clear,
            parse_me_drain_role(r#"{"state":"clear"}"#),
        );
        assert_eq!(update, RoleUpdate::Keep);
        assert_eq!(apply_role_update(ci, update), (ci, false));
        // The next read that serves the key does replace it.
        let reopened = role_update_for(
            &DrainObservation::Clear,
            parse_me_drain_role(r#"{"state":"clear","dispatch_role":"workhorse"}"#),
        );
        assert_eq!(
            apply_role_update(ci, reopened),
            (Some(DispatchRole::Workhorse), true)
        );
    }

    #[test]
    fn an_absent_or_open_role_keeps_todays_behaviour_exactly() {
        let drained = CoordDrainState::Drained {
            until: None,
            reason: Some("rebuild".into()),
        };
        let unknown = CoordDrainState::Unknown {
            since: t0(),
            cause: "x".into(),
        };
        for role in [None, Some(DispatchRole::Workhorse)] {
            for state in [
                CoordDrainState::Clear,
                CoordDrainState::NotEnrolled { why: "x" },
                drained.clone(),
                unknown.clone(),
            ] {
                for o in SpawnOrigin::ALL {
                    assert_eq!(
                        gate_with_role_at(&state, role, o, t0()),
                        gate_for_at(&state, o, t0()),
                        "{role:?} {state:?} {o:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_role_is_reported_over_a_drain_when_both_close() {
        let drained = CoordDrainState::Drained {
            until: None,
            reason: None,
        };
        match gate_with_role_at(
            &drained,
            Some(DispatchRole::CiNode),
            SpawnOrigin::Steward,
            t0(),
        ) {
            DrainGate::Defer { class, .. } => assert_eq!(class, DeferClass::RoleClosed),
            other => panic!("{other:?}"),
        }
        assert_eq!(DeferClass::RoleClosed.code(), "role_closes_agent_lane");
        assert_eq!(DeferClass::RoleClosed.label(), "role_closed");
    }

    #[test]
    fn a_failed_read_keeps_the_role_and_a_successful_read_replaces_it() {
        let ci = Some(DispatchRole::CiNode);
        assert_eq!(apply_role_update(ci, RoleUpdate::Keep), (ci, false));
        assert_eq!(apply_role_update(ci, RoleUpdate::Set(ci)), (ci, false));
        assert_eq!(apply_role_update(None, RoleUpdate::Set(ci)), (ci, true));
        // A served role this build does not know: today's behaviour.
        assert_eq!(apply_role_update(ci, RoleUpdate::Set(None)), (None, true));
    }

    #[test]
    fn the_snapshot_says_autonomous_spawns_are_off_under_a_closing_role() {
        let mut inner = Inner {
            tracker: DrainTracker::new(DateTime::<Utc>::UNIX_EPOCH),
            last_fold: Instant::now(),
            last_fold_at: DateTime::<Utc>::UNIX_EPOCH,
            deferred: BTreeSet::new(),
            deferred_capped: false,
            last_emitted: None,
            folded_once: true,
            dispatch_role: Some(DispatchRole::CiNode),
        };
        let snap = snapshot_of(&CoordDrainState::Clear, &inner);
        assert_eq!(
            snap.state, "clear",
            "the role never rewrites the drain state"
        );
        assert!(!snap.autonomous_spawns_allowed);
        assert!(snap.agent_lane_closed_by_role);
        assert_eq!(snap.dispatch_role, Some("ci_node"));

        inner.dispatch_role = None;
        let snap = snapshot_of(&CoordDrainState::Clear, &inner);
        assert!(snap.autonomous_spawns_allowed);
        assert!(!snap.agent_lane_closed_by_role);
        assert_eq!(snap.dispatch_role, None);
    }

    /// The other half of what a `ci_node` box needs: the role closes the AGENT
    /// lane only, so coord-dispatched CI builds keep running. They do because
    /// the CI-node lane (`src/ci_node/`) never consults this gate or the spawn
    /// authorization at all. A tripwire on DIRECT references only (a helper
    /// elsewhere that gates internally would not trip it): a CI lane that
    /// started asking the drain gate would be closed by `ci_node`, the very
    /// role that exists to run it.
    #[test]
    fn the_ci_node_lane_never_consults_the_agent_lane_gate() {
        fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("read ci_node dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    rs_files(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ci_node");
        let mut files = Vec::new();
        rs_files(&root, &mut files);
        assert!(files.len() >= 5, "found only {files:?} under {root:?}");
        for file in &files {
            let text = std::fs::read_to_string(file).expect("read ci_node source");
            for needle in [
                "coord_drain_state",
                "agent_authorization",
                "drain_gate",
                "authorize_spawn",
                "SpawnOrigin",
            ] {
                assert!(
                    !text.contains(needle),
                    "{} references `{needle}`: the CI-node lane must stay outside the \
                     agent-lane gate, or a `ci_node` dispatch role closes CI too",
                    file.display()
                );
            }
        }
    }
}
