//! Shepherd slot leases — the PURE core of claim-first spawning.
//!
//! Phase 8 of plan `2026-07-17-session-autonomy-fabric.md`, design decision D3.
//! This module is the direct descendant of incident #1025 (the unlandable
//! shepherd spawn storm: 89 agents spawned against one PR because the tier-0
//! dedup CHECK and the tier-1 RECORD disagreed about who had already claimed
//! the work).
//!
//! ## The rule this module exists to make structural
//!
//! **NEVER check-then-spawn. ALWAYS claim-then-spawn.**
//!
//! "Is a shepherd already running?" → spawn-if-not is a TOCTOU race with a
//! window as wide as the check's round-trip. Every runner in the fleet ticks
//! concurrently, so every runner can observe "no shepherd" in the same window
//! and every runner then spawns one. The check is not merely unreliable; it is
//! unfixable — no amount of tightening closes a window that exists by
//! construction.
//!
//! The lease inverts it. Acquiring is an ATOMIC `SET NX PX` in coord's Redis
//! (`claims::acquire`), so exactly one caller can win a given slot, and the
//! WINNING IS THE PERMISSION. There is no separate check to disagree with.
//! [`gate_action`] is where that becomes structural rather than conventional:
//! the supervisor cannot spawn without a lease because the pure decision core
//! rewrites a lease-less spawn into [`Action::None`].
//!
//! ## Why reuse `claims` instead of a new marker table (D3)
//!
//! #1025's fix produced a claim-first marker, but it is PR-shepherd-specific
//! (`coord.pr_unlandable_attempts`). The general lease primitive already
//! exists: `claims::acquire` (SET NX PX) / `heartbeat` (returns `ok`/`stolen`)
//! / `release`, with a `coord.claims_audit` trail for free. D3: wire the
//! existing primitive, don't reinvent it.
//!
//! ## Lifecycle
//!
//! ```text
//! acquire(role:<tenant>:<role>:<slot>)  ──claimed/renewed──> spawn permitted
//!                                       ──held─────────────> another live
//!                                                             shepherd owns
//!                                                             this slot: DO
//!                                                             NOTHING
//! heartbeat every tick while alive      ──ok───────────────> keep it
//!                                       ──stolen───────────> we lost it
//! release on clean stop                                       (frees the slot
//!                                                              immediately)
//! hold + tab dead past the orphan grace ────────────────────> release (a peer
//!                                                              can fill it)
//! hold + slot index >= a coord-lowered count ───────────────> release, re-walk
//! ```
//!
//! ## The hold tracks the tab
//!
//! Plan `2026-11-05-looping-agent-slot-lease-tracks-tab-liveness`. A slot
//! licenses a TAB. Renewing a slot whose tab is gone strands it: every peer
//! sees `held`, nobody fills it, and coord's placement view reports
//! `holder_without_live_session`. So the hold is conditional on the tab
//! ([`hold_decision`]): a tab that is alive (or being boot-restored) keeps its
//! slot unconditionally; a tab that has been dead for longer than
//! [`orphan_grace_ms`] since it last PROVED itself releases it. And a runner
//! only walks for a slot it can use ([`acquire_permitted`]): to adopt a live
//! tab, or to spawn one it is able and allowed to spawn.
//!
//! A dead runner stops heartbeating, its lease expires on TTL, and the freed
//! slot is acquired by exactly ONE surviving supervisor — one respawn, not a
//! storm, because the atomic acquire admits exactly one winner.

use super::desired_state::DesiredSource;
use super::policy::{self, Action, Liveness};

/// Coord `ClaimKind` used for slot leases (snake_case wire form).
///
/// `semantic_resource` is the correct existing kind, not a convenient one: it
/// is documented as "a logically-named single-writer resource that lives ACROSS
/// files … reserve a logical slot before you edit the things that materialize
/// it", with the canonical key shape `<class>:<name>` and — critically — the
/// generic `SET NX` mutex IS the reservation (it runs no alembic sibling-fork
/// query). A shepherd slot is exactly that: a logical single-writer slot whose
/// materialization is a spawned session. Adding a bespoke `ClaimKind` would
/// mean a coord deploy to teach it a TTL the caller can already pass.
pub const LEASE_CLAIM_KIND: &str = "semantic_resource";

/// Slot-lease TTL.
///
/// Passed EXPLICITLY rather than taking `semantic_resource`'s 1800s default,
/// which is tuned for "hold while a human composes a change and opens a PR".
/// A shepherd slot is machine-held and heartbeated every supervisor tick (~5s),
/// so the only thing the TTL sizes is **how long a dead runner's slot stays
/// unfillable**. 1800s would strand a tenant's shepherd for 30 minutes after a
/// runner crash; 120s recovers in about two minutes while still tolerating
/// ~24 consecutive missed heartbeats — far more slack than any transient coord
/// blip needs.
pub const LEASE_TTL_SECS: i64 = 120;

/// Outcome of an acquire attempt. Mirrors coord's `AcquireResult` discriminator
/// (`claimed` | `renewed` | `held` | …) folded to the three cases the
/// supervisor can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// `claimed` (fresh) or `renewed` (we already held it — an idempotent
    /// re-acquire that extends the TTL). Both mean: this slot is OURS.
    Acquired,
    /// `held` — a DIFFERENT owner holds this slot. Another live shepherd is
    /// filling it; do nothing at all.
    Held { current_holder: Option<String> },
    /// Transport error, non-2xx, or an unparseable body. NOT permission.
    Unavailable(String),
}

/// Outcome of a heartbeat on a lease we believe we hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// Still ours; TTL extended.
    Ok,
    /// Another owner holds this slot now — coord returned a current holder.
    Stolen,
    /// Nothing holds this slot: the grant EXPIRED, or was never taken. Coord
    /// answers this separately from `Stolen` so a lapse is not read as a theft
    /// (plan `2026-09-15-coord-claims-heartbeat-answers-an-expired-claim-as-stolen-with-no-holder`).
    /// Like `Stolen` it means we do NOT hold the slot — unlike `Stolen` it
    /// asserts no rival.
    NotHeld,
    /// Transport error. Says nothing about ownership.
    Unavailable(String),
}

/// The slot lease this supervisor currently holds for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSlot {
    pub slot: u32,
    pub resource_key: String,
    /// When this supervisor took or re-adopted the slot (`claimed`/`granted`/
    /// `renewed`). After a runner restart this is the `renewed` instant.
    pub acquired_at_ms: i64,
    /// The last tick on which the agent's tab PROVED itself alive: running
    /// past its spawn grace (the supervisor's `spawn_streak_clears`).
    pub last_proven_alive_ms: Option<i64>,
}

impl HeldSlot {
    /// The orphan-grace anchor: `max(acquired_at_ms, last_proven_alive_ms)`.
    ///
    /// Anchored on PROVEN alive, not merely alive: a respawn that reads live
    /// for ten seconds and exits (the trust-dialog / 401 pattern) must not
    /// reset the clock on every attempt.
    pub fn anchor_ms(&self) -> i64 {
        self.last_proven_alive_ms
            .map_or(self.acquired_at_ms, |p| p.max(self.acquired_at_ms))
    }
}

/// How many spawn-failure backoff steps the orphan grace covers.
const ORPHAN_GRACE_BACKOFF_STEPS: u32 = 3;

/// How long a held slot may sit over a tab that is not alive before it is
/// released: one full spawn grace plus the first three spawn-failure backoffs
/// (`(90 + 30 + 60 + 120) s = 300 s` at the default policy). Derived from the
/// existing policy fields rather than a new knob.
///
/// A routine `DeathRespawn` never comes near it — the tab is respawned on the
/// tick it dies and reads alive again within a tick — so only a holder that
/// has failed to keep ANY tab alive for about five minutes loses the slot.
pub fn orphan_grace_ms(spawn_grace_secs: u64) -> i64 {
    let backoffs: u64 = (1..=ORPHAN_GRACE_BACKOFF_STEPS)
        .map(policy::spawn_backoff_secs)
        .sum();
    let secs = spawn_grace_secs.saturating_add(backoffs);
    i64::try_from(secs).unwrap_or(i64::MAX).saturating_mul(1000)
}

/// The per-tick observation the supervisor hands the lease layer. Resolved
/// ONCE per tick, before the lease reconcile, so the lease and the decision
/// core judge the same tab (two reads per tick could disagree — the #1025
/// shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTickObs {
    pub now_ms: i64,
    pub liveness: Liveness,
    /// The tab proved itself this tick: `Live` and past its spawn grace.
    pub proven_alive: bool,
    /// This runner could spawn a tab this tick: the agent registry permits it,
    /// the device drain allows it, and the spawn-failure backoff has elapsed.
    pub spawn_able: bool,
    pub spawn_grace_secs: u64,
    pub source: DesiredSource,
    pub slots: u32,
}

/// Why a held slot is being given up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseReason {
    /// The tab has not been alive for longer than the orphan grace.
    TabNotAlive,
    /// Coord lowered the count below this slot's index.
    OutOfRange,
}

/// What to do with a slot we hold, this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldDecision {
    Keep,
    Release(ReleaseReason),
}

/// **Should a slot we hold be kept this tick?** Pure; evaluated before the
/// heartbeat.
///
/// 1. A slot index at or above a COORD-sourced count is released
///    (`OutOfRange`): the count was lowered. Only under `Coord` — the
///    local-registry fallback always reports one slot, so a failed
///    desired-state read must never release slot 1.
/// 2. A tab that is alive (`Live` or `PendingRestore`) keeps its slot,
///    unconditionally: never freed by an authz refusal, a drain, a backoff or
///    anything else.
/// 3. Otherwise (`Dead`/`Exited`) the slot is kept while
///    `now - anchor < orphan_grace` and released (`TabNotAlive`) once the
///    grace has passed. A clock that runs backwards keeps.
pub fn hold_decision(held_slot: u32, anchor_ms: i64, obs: &LeaseTickObs) -> HoldDecision {
    if obs.source == DesiredSource::Coord && held_slot >= obs.slots {
        return HoldDecision::Release(ReleaseReason::OutOfRange);
    }
    if obs.liveness.tab_alive() {
        return HoldDecision::Keep;
    }
    if obs.now_ms.saturating_sub(anchor_ms) >= orphan_grace_ms(obs.spawn_grace_secs) {
        HoldDecision::Release(ReleaseReason::TabNotAlive)
    } else {
        HoldDecision::Keep
    }
}

/// Whether `now` is still inside the cooldown that follows an orphan release
/// (one lease TTL). At exactly one TTL the cooldown is over.
pub fn in_orphan_cooldown(now_ms: i64, last_orphan_release_ms: Option<i64>) -> bool {
    last_orphan_release_ms.is_some_and(|t| now_ms.saturating_sub(t) < LEASE_TTL_SECS * 1000)
}

/// **May this tick walk the candidate slots at all?** Pure.
///
/// * A live (or restoring) tab may always adopt a free slot: adopting is not a
///   spawn, so neither the agent registry nor the drain governs it.
/// * Otherwise only a runner that could actually spawn may take a slot, and
///   not inside the cooldown after it released one for a dead tab. Without
///   that, a runner whose spawns keep failing — or are refused with no backoff
///   (the resource guard, an unread trust dial) — would re-take the slot it
///   just gave up on the next tick.
pub fn acquire_permitted(obs: &LeaseTickObs, last_orphan_release_ms: Option<i64>) -> bool {
    obs.liveness.tab_alive()
        || (obs.spawn_able && !in_orphan_cooldown(obs.now_ms, last_orphan_release_ms))
}

/// What one tick's acquire walk established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkVerdict {
    Acquired {
        slot: u32,
    },
    /// Every candidate answered `held`. The only authoritative "no room" answer.
    AllHeld,
    /// A transport/parse failure stopped the walk. Never evidence.
    Unavailable,
    /// The walk did not run (not permitted, no identity, not running, or we
    /// already hold a slot).
    NotWalked,
}

/// Pure fold of a walk's ordered outcomes. Stops at the first `Acquired` or
/// `Unavailable`, exactly as the supervisor's loop does; nothing observed is
/// `NotWalked`.
pub fn walk_verdict(outcomes: &[(u32, AcquireOutcome)]) -> WalkVerdict {
    if outcomes.is_empty() {
        return WalkVerdict::NotWalked;
    }
    for (slot, outcome) in outcomes {
        match outcome {
            AcquireOutcome::Acquired => return WalkVerdict::Acquired { slot: *slot },
            AcquireOutcome::Unavailable(_) => return WalkVerdict::Unavailable,
            AcquireOutcome::Held { .. } => {}
        }
    }
    WalkVerdict::AllHeld
}

/// What the lease reconcile reports to the supervisor for one tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseTick {
    /// This supervisor holds a slot lease right now.
    pub holds: bool,
    /// What this tick's acquire walk established (`NotWalked` when it did not
    /// run — including every tick on which we already held a slot).
    pub walk: WalkVerdict,
}

impl LeaseTick {
    /// A tick on which we hold no slot and did not walk.
    pub const NOT_HELD: LeaseTick = LeaseTick {
        holds: false,
        walk: WalkVerdict::NotWalked,
    };
    /// A tick on which we kept the slot we already held.
    pub const HELD: LeaseTick = LeaseTick {
        holds: true,
        walk: WalkVerdict::NotWalked,
    };

    /// The report for a tick whose walk ran.
    pub fn walked(walk: WalkVerdict) -> LeaseTick {
        LeaseTick {
            holds: matches!(walk, WalkVerdict::Acquired { .. }),
            walk,
        }
    }
}

/// Build the canonical slot resource key: `role:<tenant>:<role>:<slot>` (D3).
///
/// `<class>` is `role`, matching `semantic_resource`'s `<class>:<name>`
/// convention (coord stores the whole string opaquely and never splits past the
/// kind prefix, so the inner colons are free). The tenant is IN the key because
/// the lease is a per-tenant fleet invariant: two tenants each wanting one
/// shepherd must not contend for one slot.
pub fn slot_resource_key(tenant_id: &str, role: &str, slot: u32) -> String {
    format!("role:{tenant_id}:{role}:{slot}")
}

/// Which slots to attempt this tick, in order.
///
/// Holding a slot already → try only that one (the re-acquire/heartbeat path).
/// Otherwise every slot `0..slots`, ascending: the atomic acquire makes the
/// ORDER irrelevant to correctness (it only affects which runner lands on which
/// index), so a deterministic order keeps the behavior reproducible and the
/// tests honest.
pub fn candidate_slots(slots: u32, held: Option<u32>) -> Vec<u32> {
    match held {
        Some(s) => vec![s],
        None => (0..slots).collect(),
    }
}

/// **The claim-first gate.** Rewrites any session-CREATING action into
/// [`Action::None`] unless this supervisor holds a slot lease.
///
/// This is the structural enforcement of the #1025 lesson, and it is why the
/// supervisor cannot regress into check-then-spawn by accident: the spawn is
/// not guarded by a convention a future edit might forget, it is unreachable
/// without a lease.
///
/// * [`Action::Spawn`] — creates a session. Requires the lease.
/// * [`Action::Relaunch`] — closes the tab and creates a REPLACEMENT session.
///   Also requires the lease: without it we would either spawn an unlicensed
///   session or, worse, close a shepherd and fail to bring it back.
/// * [`Action::Nudge`] / [`Action::None`] — drive an already-licensed tab and
///   create nothing. Ungated, so a coord blip mid-cycle can't wedge a live
///   shepherd's cadence.
pub fn gate_action(action: Action, holds_lease: bool) -> Action {
    if holds_lease {
        return action;
    }
    match action {
        Action::Spawn(_) | Action::Relaunch(_) => Action::None,
        other => other,
    }
}

/// Does this acquire outcome grant permission to spawn? ONLY `Acquired` does.
///
/// Spelled out as its own predicate so the "unavailable is not permission"
/// invariant has exactly one home. A coord we cannot reach must never be read
/// as a free slot — that is the failure mode that manufactures storms.
pub fn spawn_permitted(outcome: &AcquireOutcome) -> bool {
    matches!(outcome, AcquireOutcome::Acquired)
}

/// Fold a heartbeat outcome into "do we still believe we hold this lease?".
///
/// The asymmetry is deliberate:
/// * `Stolen` and `NotHeld` are both COORD'S AUTHORITATIVE ANSWER that we do
///   not hold it → drop it. They differ only in whether a rival exists, which
///   matters for the operator-facing message, never for this predicate.
/// * `Unavailable` is OUR failure to ask → keep believing we hold it. Dropping
///   on a transport blip would make every runner release its slot during a coord
///   hiccup and then all race to re-acquire — self-inflicted churn. If coord is
///   genuinely gone, our TTL expires anyway and the slot frees itself, which is
///   the same outcome via the mechanism that is actually authoritative.
pub fn still_held_after_heartbeat(outcome: &HeartbeatOutcome) -> bool {
    match outcome {
        HeartbeatOutcome::Ok => true,
        HeartbeatOutcome::Stolen => false,
        HeartbeatOutcome::NotHeld => false,
        HeartbeatOutcome::Unavailable(_) => true,
    }
}

/// Parse coord's `/claims/acquire` `result` discriminator.
pub fn parse_acquire_result(result: &str, current_holder: Option<String>) -> AcquireOutcome {
    match result {
        "claimed" | "renewed" => AcquireOutcome::Acquired,
        "held" => AcquireOutcome::Held { current_holder },
        // Topic/tenant/invalid results can't arise for this call shape (no
        // topic, no explicit tenant), but an unknown discriminator is NEVER
        // silently treated as permission.
        other => AcquireOutcome::Unavailable(format!("unexpected acquire result `{other}`")),
    }
}

/// Parse coord's `/claims/heartbeat` `result` discriminator.
pub fn parse_heartbeat_result(result: &str) -> HeartbeatOutcome {
    match result {
        "ok" => HeartbeatOutcome::Ok,
        "stolen" => HeartbeatOutcome::Stolen,
        // An expired or absent claim. coord spelled this `stolen` with a null
        // holder until the plan above; an unmapped word here would fall to
        // `Unavailable`, which `still_held_after_heartbeat` reads as STILL
        // HELD — so a lapse would leave this supervisor running on a slot a
        // peer can take. That is the regression this arm exists to prevent.
        "not_held" => HeartbeatOutcome::NotHeld,
        other => HeartbeatOutcome::Unavailable(format!("unexpected heartbeat result `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::looping_agent::policy::{RelaunchReason, SpawnReason};

    #[test]
    fn resource_key_is_the_d3_shape() {
        assert_eq!(
            slot_resource_key("11111111-1111-1111-1111-111111111111", "merge_shepherd", 0),
            "role:11111111-1111-1111-1111-111111111111:merge_shepherd:0"
        );
    }

    #[test]
    fn resource_keys_are_distinct_per_tenant_and_per_slot() {
        // Two tenants each wanting one shepherd must not contend for one slot.
        assert_ne!(
            slot_resource_key("tenant-a", "merge_shepherd", 0),
            slot_resource_key("tenant-b", "merge_shepherd", 0)
        );
        assert_ne!(
            slot_resource_key("t", "merge_shepherd", 0),
            slot_resource_key("t", "merge_shepherd", 1)
        );
        assert_ne!(
            slot_resource_key("t", "merge_shepherd", 0),
            slot_resource_key("t", "merge_fixer", 0)
        );
    }

    // -- Claim-first ordering: NO SPAWN WITHOUT AN ACQUIRE ------------------

    #[test]
    fn spawn_is_impossible_without_a_lease() {
        // The heart of the phase. Every session-creating action is rewritten to
        // None when we hold no lease.
        for action in [
            Action::Spawn(SpawnReason::FirstSpawn),
            Action::Spawn(SpawnReason::DeathRespawn),
            Action::Relaunch(RelaunchReason::ContextLow),
            Action::Relaunch(RelaunchReason::CycleBudget),
        ] {
            assert_eq!(
                gate_action(action, false),
                Action::None,
                "{action:?} must be impossible without a slot lease (claim-first)"
            );
        }
    }

    #[test]
    fn spawn_passes_through_once_the_lease_is_held() {
        assert_eq!(
            gate_action(Action::Spawn(SpawnReason::FirstSpawn), true),
            Action::Spawn(SpawnReason::FirstSpawn)
        );
        assert_eq!(
            gate_action(Action::Relaunch(RelaunchReason::ContextLow), true),
            Action::Relaunch(RelaunchReason::ContextLow)
        );
    }

    #[test]
    fn non_creating_actions_are_never_gated() {
        // A live shepherd's cadence must not wedge just because a lease call
        // failed this tick.
        assert_eq!(gate_action(Action::Nudge, false), Action::Nudge);
        assert_eq!(gate_action(Action::None, false), Action::None);
    }

    #[test]
    fn only_an_acquired_outcome_grants_permission() {
        assert!(spawn_permitted(&AcquireOutcome::Acquired));
        assert!(!spawn_permitted(&AcquireOutcome::Held {
            current_holder: Some("other-machine".into())
        }));
        // "I couldn't reach coord" is NOT "the slot is free" — this assertion
        // is the storm-prevention invariant in one line.
        assert!(!spawn_permitted(&AcquireOutcome::Unavailable(
            "connection refused".into()
        )));
    }

    #[test]
    fn held_means_another_live_shepherd_owns_the_slot_so_do_nothing() {
        let outcome = parse_acquire_result("held", Some("machine-b".into()));
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                current_holder: Some("machine-b".into())
            }
        );
        assert!(
            !spawn_permitted(&outcome),
            "a held slot must never be spawned into — that IS the storm"
        );
        assert_eq!(
            gate_action(Action::Spawn(SpawnReason::FirstSpawn), false),
            Action::None
        );
    }

    #[test]
    fn acquire_discriminators_parse_and_unknown_is_never_permission() {
        assert_eq!(
            parse_acquire_result("claimed", None),
            AcquireOutcome::Acquired
        );
        // `renewed` = we already hold it (restart / re-acquire) — still ours.
        assert_eq!(
            parse_acquire_result("renewed", None),
            AcquireOutcome::Acquired
        );
        assert!(!spawn_permitted(&parse_acquire_result(
            "tenant_not_bound",
            None
        )));
        assert!(!spawn_permitted(&parse_acquire_result("", None)));
    }

    #[test]
    fn heartbeat_stolen_drops_the_lease_but_a_transport_error_does_not() {
        assert!(still_held_after_heartbeat(&parse_heartbeat_result("ok")));
        assert!(
            !still_held_after_heartbeat(&parse_heartbeat_result("stolen")),
            "stolen is coord's authoritative answer that we do not hold it"
        );
        assert!(
            still_held_after_heartbeat(&HeartbeatOutcome::Unavailable("timeout".into())),
            "a transport blip must not make every runner drop and re-race its slot"
        );
    }

    /// An EXPIRED claim drops the lease exactly as a stolen one does, and is
    /// parsed as its own verdict rather than falling through to `Unavailable`.
    ///
    /// The second assertion is the load-bearing one: `Unavailable` reads as
    /// STILL HELD, so an unmapped `not_held` would leave this supervisor
    /// believing it owns a slot coord has just said nobody owns — and a peer
    /// acquiring the same slot then runs a second agent on it. A mutation
    /// deleting the `"not_held"` arm turns this red.
    #[test]
    fn heartbeat_not_held_drops_the_lease_and_is_not_an_unavailable() {
        assert_eq!(
            parse_heartbeat_result("not_held"),
            HeartbeatOutcome::NotHeld
        );
        assert!(
            !still_held_after_heartbeat(&parse_heartbeat_result("not_held")),
            "an expired claim is not held, however politely coord says so"
        );
        assert_ne!(
            parse_heartbeat_result("not_held"),
            HeartbeatOutcome::Stolen,
            "an expiry must stay distinguishable from a theft on the wire"
        );
    }

    #[test]
    fn candidate_slots_targets_only_the_held_slot_once_we_hold_one() {
        assert_eq!(candidate_slots(4, Some(2)), vec![2]);
        assert_eq!(candidate_slots(3, None), vec![0, 1, 2]);
        assert!(
            candidate_slots(0, None).is_empty(),
            "zero slots = nothing to chase"
        );
    }

    // -- Fleet simulation: the storm regression ----------------------------

    /// A minimal model of coord's `SET NX PX` slot mutex — the ONLY property
    /// the storm argument depends on: at most one owner per key at a time, and
    /// expiry frees the key.
    struct FakeCoordMutex {
        owners: std::collections::HashMap<String, (String, i64)>, // key -> (owner, expires_at)
    }

    impl FakeCoordMutex {
        fn new() -> Self {
            Self {
                owners: std::collections::HashMap::new(),
            }
        }
        fn acquire(&mut self, key: &str, owner: &str, now: i64) -> AcquireOutcome {
            match self.owners.get(key) {
                Some((holder, expires)) if *expires > now && holder != owner => {
                    AcquireOutcome::Held {
                        current_holder: Some(holder.clone()),
                    }
                }
                // Free, expired, or already ours (renew).
                _ => {
                    self.owners
                        .insert(key.to_string(), (owner.to_string(), now + LEASE_TTL_SECS));
                    AcquireOutcome::Acquired
                }
            }
        }
    }

    /// One supervisor's claim-first tick, exactly as the real one composes it:
    /// acquire a candidate slot, then gate the spawn on the outcome. Returns
    /// whether it spawned.
    fn supervisor_tick(
        coord: &mut FakeCoordMutex,
        machine: &str,
        slots: u32,
        held: &mut Option<u32>,
        now: i64,
    ) -> bool {
        for slot in candidate_slots(slots, *held) {
            let key = slot_resource_key("tenant-x", "merge_shepherd", slot);
            let outcome = coord.acquire(&key, machine, now);
            if spawn_permitted(&outcome) {
                *held = Some(slot);
                let action = gate_action(Action::Spawn(SpawnReason::FirstSpawn), true);
                return matches!(action, Action::Spawn(_));
            }
        }
        // No lease → the gate makes a spawn structurally impossible.
        matches!(
            gate_action(Action::Spawn(SpawnReason::FirstSpawn), false),
            Action::Spawn(_)
        )
    }

    #[test]
    fn five_runners_racing_one_slot_produce_exactly_one_shepherd() {
        // The #1025 storm, replayed. Five supervisors tick concurrently against
        // N=1. Check-then-spawn gives five shepherds; claim-first gives one.
        let mut coord = FakeCoordMutex::new();
        let mut held = [None; 5];
        let spawns: usize = (0..5)
            .filter(|i| supervisor_tick(&mut coord, &format!("machine-{i}"), 1, &mut held[*i], 0))
            .count();
        assert_eq!(spawns, 1, "exactly one runner may fill a single slot");

        // And it stays one across subsequent ticks — the holder renews, the
        // losers keep losing. No drift, no second shepherd.
        let spawns: usize = (0..5)
            .filter(|i| supervisor_tick(&mut coord, &format!("machine-{i}"), 1, &mut held[*i], 5))
            .count();
        assert_eq!(spawns, 1, "the holder renews; nobody else may spawn");
    }

    #[test]
    fn n_slots_admit_exactly_n_shepherds_fleet_wide() {
        let mut coord = FakeCoordMutex::new();
        let mut held = [None; 5];
        let spawns: usize = (0..5)
            .filter(|i| supervisor_tick(&mut coord, &format!("machine-{i}"), 2, &mut held[*i], 0))
            .count();
        assert_eq!(
            spawns, 2,
            "count=2 must admit exactly two shepherds fleet-wide"
        );
    }

    #[test]
    fn an_expired_lease_yields_exactly_one_respawn_not_a_storm() {
        // machine-0 wins the only slot, then its runner DIES (it stops
        // heartbeating). The lease expires. The four survivors all notice on
        // the same tick — the #1025 shape exactly.
        let mut coord = FakeCoordMutex::new();
        let mut held: Vec<Option<u32>> = vec![None; 5];
        assert!(supervisor_tick(&mut coord, "machine-0", 1, &mut held[0], 0));

        // Before expiry, nobody may take over.
        let takeovers: usize = (1..5)
            .filter(|i| supervisor_tick(&mut coord, &format!("machine-{i}"), 1, &mut held[*i], 10))
            .count();
        assert_eq!(takeovers, 0, "a live lease must not be stolen");

        // After expiry (machine-0 is gone and never renews), the survivors race.
        let now = LEASE_TTL_SECS + 1;
        let respawns: usize = (1..5)
            .filter(|i| supervisor_tick(&mut coord, &format!("machine-{i}"), 1, &mut held[*i], now))
            .count();
        assert_eq!(
            respawns, 1,
            "an expired lease must yield EXACTLY ONE respawn — four would be the #1025 storm"
        );
    }

    #[test]
    fn a_restarted_runner_renews_its_own_lease_rather_than_deadlocking_on_it() {
        // machine-0 holds the slot, then its supervisor restarts and forgets
        // (`held = None`). Its re-acquire must return Acquired (renew), NOT
        // Held-by-itself — otherwise a restart would permanently strand the
        // slot behind the runner's own lease.
        let mut coord = FakeCoordMutex::new();
        let mut held = None;
        assert!(supervisor_tick(&mut coord, "machine-0", 1, &mut held, 0));

        let mut held_after_restart = None;
        assert!(
            supervisor_tick(&mut coord, "machine-0", 1, &mut held_after_restart, 5),
            "a runner must be able to re-acquire (renew) its OWN lease after a restart"
        );
        assert_eq!(held_after_restart, Some(0));
    }

    // -- The hold tracks the tab (plan 2026-11-05-…-tracks-tab-liveness) ----

    const GRACE_SECS: u64 = 90;
    const TICK_MS: i64 = 5_000;
    const TTL_MS: i64 = LEASE_TTL_SECS * 1000;

    fn obs(now_ms: i64, liveness: Liveness) -> LeaseTickObs {
        LeaseTickObs {
            now_ms,
            liveness,
            proven_alive: false,
            spawn_able: true,
            spawn_grace_secs: GRACE_SECS,
            source: DesiredSource::Coord,
            slots: 1,
        }
    }

    fn held() -> AcquireOutcome {
        AcquireOutcome::Held {
            current_holder: None,
        }
    }

    #[test]
    fn orphan_grace_is_spawn_grace_plus_three_backoffs() {
        assert_eq!(orphan_grace_ms(90), 300_000);
        // One ordinary failed respawn (grace + first backoff) never costs the slot.
        assert!(
            orphan_grace_ms(90)
                > (GRACE_SECS as i64) * 1000 + (policy::spawn_backoff_secs(1) as i64) * 1000
        );
        assert_eq!(
            orphan_grace_ms(u64::MAX),
            i64::MAX,
            "saturates, never panics"
        );
    }

    #[test]
    fn the_anchor_is_the_later_of_acquire_and_last_proof() {
        let mut h = HeldSlot {
            slot: 0,
            resource_key: "k".into(),
            acquired_at_ms: 1_000,
            last_proven_alive_ms: None,
        };
        assert_eq!(h.anchor_ms(), 1_000);
        h.last_proven_alive_ms = Some(500);
        assert_eq!(h.anchor_ms(), 1_000, "a proof older than the acquire");
        h.last_proven_alive_ms = Some(9_000);
        assert_eq!(h.anchor_ms(), 9_000);
    }

    #[test]
    fn a_live_or_pending_tab_keeps_its_slot_at_any_age() {
        // The authz-refusal invariant: tab liveness — never `spawn_able` — is
        // what keeps the slot.
        let ten_days = 10 * 24 * 3_600 * 1000;
        for l in [Liveness::Live, Liveness::PendingRestore] {
            let o = LeaseTickObs {
                spawn_able: false,
                ..obs(ten_days, l)
            };
            assert_eq!(hold_decision(0, 0, &o), HoldDecision::Keep, "{l:?}");
        }
    }

    #[test]
    fn a_dead_tab_keeps_its_slot_inside_the_grace() {
        let g = orphan_grace_ms(GRACE_SECS);
        for l in [Liveness::Dead, Liveness::Exited] {
            assert_eq!(hold_decision(0, 0, &obs(g - 1, l)), HoldDecision::Keep);
        }
    }

    #[test]
    fn a_dead_tab_releases_past_the_grace() {
        let g = orphan_grace_ms(GRACE_SECS);
        for l in [Liveness::Dead, Liveness::Exited] {
            assert_eq!(
                hold_decision(0, 1_000, &obs(1_000 + g, l)),
                HoldDecision::Release(ReleaseReason::TabNotAlive),
                "{l:?}: the grace boundary is inclusive"
            );
        }
    }

    #[test]
    fn clock_skew_never_releases() {
        assert_eq!(
            hold_decision(0, 10_000_000, &obs(0, Liveness::Dead)),
            HoldDecision::Keep
        );
    }

    #[test]
    fn an_out_of_range_slot_releases_only_under_a_coord_count() {
        assert_eq!(
            hold_decision(1, 0, &obs(0, Liveness::Live)),
            HoldDecision::Release(ReleaseReason::OutOfRange),
            "even with a live tab"
        );
        let local = LeaseTickObs {
            source: DesiredSource::LocalRegistry,
            ..obs(0, Liveness::Live)
        };
        assert_eq!(
            hold_decision(1, 0, &local),
            HoldDecision::Keep,
            "a failed desired-state read (local fallback = 1 slot) never releases slot 1"
        );
        assert_eq!(
            hold_decision(0, 0, &obs(0, Liveness::Live)),
            HoldDecision::Keep
        );
    }

    #[test]
    fn acquire_permitted_matrix() {
        let now = 1_000_000;
        // Alive → adopt, even when this runner could not spawn.
        for l in [Liveness::Live, Liveness::PendingRestore] {
            let o = LeaseTickObs {
                spawn_able: false,
                ..obs(now, l)
            };
            assert!(acquire_permitted(&o, Some(now)), "{l:?}");
        }
        for l in [Liveness::Dead, Liveness::Exited] {
            assert!(acquire_permitted(&obs(now, l), None));
            let refused = LeaseTickObs {
                spawn_able: false,
                ..obs(now, l)
            };
            assert!(!acquire_permitted(&refused, None));
            // Inside the cooldown after an orphan release.
            assert!(!acquire_permitted(&obs(now, l), Some(now - TTL_MS + 1)));
            // The boundary: exactly one TTL later the cooldown is over.
            assert!(acquire_permitted(&obs(now, l), Some(now - TTL_MS)));
        }
    }

    #[test]
    fn walk_verdict_folds() {
        let unavailable = || AcquireOutcome::Unavailable("timeout".into());
        assert_eq!(
            walk_verdict(&[(0, held()), (1, held())]),
            WalkVerdict::AllHeld
        );
        assert_eq!(
            walk_verdict(&[(0, held()), (1, unavailable())]),
            WalkVerdict::Unavailable
        );
        assert_eq!(
            walk_verdict(&[(0, held()), (1, AcquireOutcome::Acquired)]),
            WalkVerdict::Acquired { slot: 1 }
        );
        assert_eq!(
            walk_verdict(&[(0, unavailable())]),
            WalkVerdict::Unavailable
        );
        assert_eq!(walk_verdict(&[]), WalkVerdict::NotWalked);
        // A `held` naming nobody is still held.
        assert_eq!(
            walk_verdict(&[(
                0,
                AcquireOutcome::Held {
                    current_holder: None
                }
            )]),
            WalkVerdict::AllHeld
        );
    }

    // -- Fleet simulations --------------------------------------------------

    /// A richer model of coord's slot mutex than [`FakeCoordMutex`]: TTLs in
    /// MILLISECONDS, heartbeat/release, and an outage switch under which every
    /// call answers `Unavailable`.
    struct SimCoord {
        owners: std::collections::HashMap<String, (String, i64)>,
        outage: bool,
    }

    impl SimCoord {
        fn new() -> Self {
            Self {
                owners: std::collections::HashMap::new(),
                outage: false,
            }
        }
        fn live_owner(&self, key: &str, now: i64) -> Option<&str> {
            self.owners
                .get(key)
                .filter(|(_, exp)| *exp > now)
                .map(|(o, _)| o.as_str())
        }
        fn acquire(&mut self, key: &str, owner: &str, now: i64) -> AcquireOutcome {
            if self.outage {
                return AcquireOutcome::Unavailable("outage".into());
            }
            match self.live_owner(key, now) {
                Some(h) if h != owner => AcquireOutcome::Held {
                    current_holder: Some(h.to_string()),
                },
                _ => {
                    self.owners
                        .insert(key.to_string(), (owner.to_string(), now + TTL_MS));
                    AcquireOutcome::Acquired
                }
            }
        }
        fn heartbeat(&mut self, key: &str, owner: &str, now: i64) -> HeartbeatOutcome {
            if self.outage {
                return HeartbeatOutcome::Unavailable("outage".into());
            }
            match self.live_owner(key, now) {
                Some(h) if h == owner => {
                    self.owners
                        .insert(key.to_string(), (owner.to_string(), now + TTL_MS));
                    HeartbeatOutcome::Ok
                }
                Some(_) => HeartbeatOutcome::Stolen,
                None => HeartbeatOutcome::NotHeld,
            }
        }
        fn release(&mut self, key: &str, owner: &str) {
            if self.outage {
                return; // best effort: the TTL is the backstop
            }
            if self.owners.get(key).is_some_and(|(o, _)| o == owner) {
                self.owners.remove(key);
            }
        }
    }

    /// How a runner's spawns behave.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SpawnBehavior {
        /// The spawn succeeds and the tab stays up.
        Ok,
        /// Every spawn fails (bad account, refused by a guard with a backoff).
        Fails,
        /// The spawn succeeds, then the tab exits one tick later (the
        /// trust-dialog / 401 pattern) — an early exit takes the backoff.
        ExitsEarly,
    }

    struct SimRunner {
        name: &'static str,
        held: Option<HeldSlot>,
        tab: Liveness,
        last_spawn_at: Option<i64>,
        spawn: SpawnBehavior,
        /// The agent registry permits a spawn (the authz opt-in).
        authz: bool,
        /// The idle probe reads turn-complete.
        idle: bool,
        failures: u32,
        next_spawn_at: i64,
        last_orphan_release: Option<i64>,
        spawns: u32,
        nudges: u32,
        releases: Vec<(i64, ReleaseReason)>,
    }

    impl SimRunner {
        fn new(name: &'static str, tab: Liveness, spawn: SpawnBehavior) -> Self {
            Self {
                name,
                held: None,
                tab,
                last_spawn_at: None,
                spawn,
                authz: true,
                idle: true,
                failures: 0,
                next_spawn_at: 0,
                last_orphan_release: None,
                spawns: 0,
                nudges: 0,
                releases: Vec::new(),
            }
        }
        /// Start the runner already holding `slot`, its tab proven at `at`.
        fn holding(mut self, slot: u32, at: i64) -> Self {
            self.held = Some(HeldSlot {
                slot,
                resource_key: key(slot),
                acquired_at_ms: at,
                last_proven_alive_ms: Some(at),
            });
            self
        }
        fn live_tab(&self) -> bool {
            self.tab == Liveness::Live
        }
    }

    fn key(slot: u32) -> String {
        slot_resource_key("tenant-x", "merge_shepherd", slot)
    }

    /// What one simulated tick did.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct SimTick {
        action: Action,
        walk: WalkVerdict,
    }

    /// One supervisor tick over the PURE core, composed in the order the real
    /// supervisor composes it: liveness first; `hold_decision`, then the
    /// heartbeat; `acquire_permitted`, then the walk and its verdict; then
    /// `decide` → `gate_action`; then the action is applied to the tab.
    fn sim_tick(
        coord: &mut SimCoord,
        r: &mut SimRunner,
        slots: u32,
        source: DesiredSource,
        now: i64,
    ) -> SimTick {
        // An early-exit tab dies on the tick after its spawn and backs off.
        if r.spawn == SpawnBehavior::ExitsEarly
            && r.tab == Liveness::Live
            && r.last_spawn_at.is_some_and(|t| t < now)
        {
            r.tab = Liveness::Exited;
            r.failures += 1;
            r.next_spawn_at = now + (policy::spawn_backoff_secs(r.failures) as i64) * 1000;
        }

        let grace_elapsed = policy::spawn_grace_elapsed(now, r.last_spawn_at, GRACE_SECS);
        let backoff_elapsed = now >= r.next_spawn_at;
        let o = LeaseTickObs {
            now_ms: now,
            liveness: r.tab,
            proven_alive: r.tab == Liveness::Live && grace_elapsed,
            spawn_able: r.authz && backoff_elapsed,
            spawn_grace_secs: GRACE_SECS,
            source,
            slots,
        };

        // -- Hold. --
        let mut holds = false;
        let mut may_walk = true;
        if let Some(h) = r.held.as_mut() {
            if o.proven_alive {
                h.last_proven_alive_ms = Some(now);
            }
            match hold_decision(h.slot, h.anchor_ms(), &o) {
                HoldDecision::Release(reason) => {
                    coord.release(&h.resource_key, r.name);
                    r.releases.push((now, reason));
                    r.held = None;
                    if reason == ReleaseReason::TabNotAlive {
                        r.last_orphan_release = Some(now);
                        may_walk = false; // never re-take the slot just given up
                    }
                }
                HoldDecision::Keep => {
                    if still_held_after_heartbeat(&coord.heartbeat(&h.resource_key, r.name, now)) {
                        holds = true;
                        may_walk = false;
                    } else {
                        r.held = None;
                    }
                }
            }
        }

        // -- Acquire walk. --
        let mut walk = WalkVerdict::NotWalked;
        if may_walk && acquire_permitted(&o, r.last_orphan_release) {
            let mut outcomes = Vec::new();
            for slot in candidate_slots(slots, None) {
                let outcome = coord.acquire(&key(slot), r.name, now);
                let stop = !matches!(outcome, AcquireOutcome::Held { .. });
                outcomes.push((slot, outcome));
                if stop {
                    break;
                }
            }
            walk = walk_verdict(&outcomes);
            if let WalkVerdict::Acquired { slot } = walk {
                r.held = Some(HeldSlot {
                    slot,
                    resource_key: key(slot),
                    acquired_at_ms: now,
                    last_proven_alive_ms: o.proven_alive.then_some(now),
                });
                holds = true;
            }
        }

        // -- Decide → gate. --
        let input = policy::TickInput {
            enabled: true,
            desired_running: true,
            tab_alive: r.tab.tab_alive(),
            ever_spawned: true,
            spawn_backoff_elapsed: backoff_elapsed,
            spawn_grace_elapsed: grace_elapsed,
            idle: r.idle,
            context_low: false,
            cadence_elapsed: true,
            cycles_since_relaunch: 1,
            relaunch_every_cycles: 1_000,
        };
        let action = gate_action(policy::decide(&input), holds);
        if !r.authz && matches!(action, Action::Spawn(_) | Action::Relaunch(_)) {
            // The supervisor's Phase 4c rewrite.
            return SimTick {
                action: Action::None,
                walk,
            };
        }

        // -- Act. --
        match action {
            Action::Spawn(_) => match r.spawn {
                SpawnBehavior::Fails => {
                    r.failures += 1;
                    r.next_spawn_at = now + (policy::spawn_backoff_secs(r.failures) as i64) * 1000;
                }
                SpawnBehavior::Ok | SpawnBehavior::ExitsEarly => {
                    r.tab = Liveness::Live;
                    r.last_spawn_at = Some(now);
                    r.spawns += 1;
                }
            },
            Action::Nudge => r.nudges += 1,
            Action::None | Action::Relaunch(_) => {}
        }
        SimTick { action, walk }
    }

    fn slotted_live_tabs(runners: &[&SimRunner]) -> usize {
        runners
            .iter()
            .filter(|r| r.held.is_some() && r.live_tab())
            .count()
    }

    /// S1: the holder's tab died and every respawn fails. It keeps the slot
    /// through the grace, releases it at the grace, a peer fills it within a
    /// tick, and the releaser does not take it back.
    #[test]
    fn a_dead_tab_holder_releases_and_a_peer_fills_the_slot() {
        let mut coord = SimCoord::new();
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        let mut r0 = SimRunner::new("r0", Liveness::Dead, SpawnBehavior::Fails).holding(0, 0);
        let mut r1 = SimRunner::new("r1", Liveness::Dead, SpawnBehavior::Ok);
        let grace = orphan_grace_ms(GRACE_SECS);

        let mut now = 0;
        while now < grace {
            sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            let t1 = sim_tick(&mut coord, &mut r1, 1, DesiredSource::Coord, now);
            assert!(r0.held.is_some(), "kept inside the grace (t={now})");
            assert_eq!(t1.walk, WalkVerdict::AllHeld);
            assert_eq!(r1.spawns, 0, "no spawn without the slot");
            now += TICK_MS;
        }
        // The tick at the grace: R0 releases, R1 acquires and spawns.
        sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
        assert_eq!(r0.releases, vec![(grace, ReleaseReason::TabNotAlive)]);
        let t1 = sim_tick(&mut coord, &mut r1, 1, DesiredSource::Coord, now);
        assert_eq!(t1.walk, WalkVerdict::Acquired { slot: 0 });
        assert_eq!(r1.spawns, 1);

        // R0 never takes it back — not in its cooldown, not after it.
        for _ in 0..200 {
            now += TICK_MS;
            sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            sim_tick(&mut coord, &mut r1, 1, DesiredSource::Coord, now);
            assert!(r0.held.is_none());
            assert_eq!(slotted_live_tabs(&[&r0, &r1]), 1);
        }
    }

    /// The cooldown, on its own: a releaser whose spawns are refused WITHOUT a
    /// backoff (resource guard, unread trust dial) stays out of the race for
    /// one TTL instead of re-taking the slot on the next tick.
    #[test]
    fn a_releaser_stays_out_of_the_race_for_one_ttl() {
        let mut coord = SimCoord::new();
        let mut r0 = SimRunner::new("r0", Liveness::Dead, SpawnBehavior::Fails).holding(0, 0);
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        // No backoff: a refusal that retries every tick.
        let grace = orphan_grace_ms(GRACE_SECS);
        let mut now = grace;
        r0.next_spawn_at = 0;
        sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
        assert!(r0.held.is_none(), "released at the grace");
        while now < grace + TTL_MS {
            r0.next_spawn_at = 0; // spawn_able stays true
            let t = sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            assert_eq!(t.walk, WalkVerdict::NotWalked, "cooldown (t={now})");
            now += TICK_MS;
        }
        r0.next_spawn_at = 0;
        let t = sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
        assert_eq!(t.walk, WalkVerdict::Acquired { slot: 0 }, "after one TTL");
    }

    /// S8: a long-lived tab dies and is respawned on the same tick. The slot
    /// is never vacated across two graces.
    #[test]
    fn a_normal_death_respawn_keeps_the_slot() {
        let mut coord = SimCoord::new();
        let hours = 6 * 3_600 * 1000;
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        let mut r0 = SimRunner::new("r0", Liveness::Live, SpawnBehavior::Ok).holding(0, 0);
        r0.last_spawn_at = Some(-hours);
        let grace = orphan_grace_ms(GRACE_SECS);
        let mut now = 0;
        while now < 2 * grace {
            if now == 60_000 {
                r0.tab = Liveness::Dead; // the routine death
            }
            sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            assert!(r0.held.is_some(), "t={now}");
            assert!(r0.releases.is_empty());
            now += TICK_MS;
        }
        assert_eq!(r0.spawns, 1, "one DeathRespawn");
        assert!(r0.live_tab());
    }

    /// S9: every respawn reads live for one tick and exits, with the backoff
    /// escalating. The release lands at last PROOF of life + grace — not when
    /// the backoff alone has outgrown the grace.
    #[test]
    fn an_early_exit_loop_releases_on_the_proven_alive_clock() {
        let mut coord = SimCoord::new();
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        // Proven alive at t=0 (an adopted tab: no spawn recorded), dies at t=5s.
        let mut r0 = SimRunner::new("r0", Liveness::Live, SpawnBehavior::ExitsEarly).holding(0, 0);
        let grace = orphan_grace_ms(GRACE_SECS);
        let mut now = 0;
        while r0.releases.is_empty() && now < 10 * grace {
            if now == TICK_MS {
                r0.tab = Liveness::Dead;
            }
            sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            now += TICK_MS;
        }
        assert!(r0.spawns >= 3, "it kept trying: {} spawns", r0.spawns);
        let (at, reason) = r0.releases[0];
        assert_eq!(reason, ReleaseReason::TabNotAlive);
        assert!(
            (grace..grace + 2 * TICK_MS).contains(&at),
            "released at {at}, expected last proof (0) + grace ({grace})"
        );
    }

    /// S10: an authz-refused runner with a live tab keeps its slot forever.
    #[test]
    fn a_refused_runner_with_a_live_tab_keeps_its_slot_forever() {
        let mut coord = SimCoord::new();
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        let mut r0 = SimRunner::new("r0", Liveness::Live, SpawnBehavior::Ok).holding(0, 0);
        r0.authz = false;
        let grace = orphan_grace_ms(GRACE_SECS);
        let mut now = 0;
        while now < 10 * grace {
            let t = sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            assert!(r0.held.is_some() && r0.releases.is_empty(), "t={now}");
            assert_eq!(t.action, Action::Nudge, "nudging is not spawning");
            now += TICK_MS;
        }
        assert!(r0.nudges > 0 && r0.spawns == 0);
    }

    /// S3 (release side): a coord outage of ten TTLs never releases a hold and
    /// never lets anybody spawn.
    #[test]
    fn a_coord_outage_never_releases_and_nobody_spawns() {
        let mut coord = SimCoord::new();
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        let mut r0 = SimRunner::new("r0", Liveness::Live, SpawnBehavior::Ok).holding(0, 0);
        let mut r1 = SimRunner::new("r1", Liveness::Live, SpawnBehavior::Ok);
        let mut r2 = SimRunner::new("r2", Liveness::Dead, SpawnBehavior::Ok);
        coord.outage = true;
        let mut now = 0;
        while now < 10 * TTL_MS {
            sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, now);
            let t1 = sim_tick(&mut coord, &mut r1, 1, DesiredSource::Coord, now);
            let t2 = sim_tick(&mut coord, &mut r2, 1, DesiredSource::Coord, now);
            assert!(r0.held.is_some(), "a heartbeat Unavailable keeps the hold");
            assert_eq!(t1.walk, WalkVerdict::Unavailable);
            assert_eq!(t2.walk, WalkVerdict::Unavailable);
            now += TICK_MS;
        }
        assert_eq!(r0.spawns + r1.spawns + r2.spawns, 0);
        assert!(r0.releases.is_empty());
    }

    /// S11 (release side): coord lowers the count 2 → 1. The holder of slot 1
    /// releases it (`OutOfRange`) and re-walks the lower slots, which are held.
    #[test]
    fn lowering_the_count_releases_the_out_of_range_slot() {
        let mut coord = SimCoord::new();
        coord.owners.insert(key(0), ("r0".into(), TTL_MS));
        coord.owners.insert(key(1), ("r1".into(), TTL_MS));
        let mut r0 = SimRunner::new("r0", Liveness::Live, SpawnBehavior::Ok).holding(0, 0);
        let mut r1 = SimRunner::new("r1", Liveness::Live, SpawnBehavior::Ok).holding(1, 0);
        sim_tick(&mut coord, &mut r0, 2, DesiredSource::Coord, 0);
        sim_tick(&mut coord, &mut r1, 2, DesiredSource::Coord, 0);
        assert!(r1.held.is_some());

        sim_tick(&mut coord, &mut r0, 1, DesiredSource::Coord, TICK_MS);
        let t1 = sim_tick(&mut coord, &mut r1, 1, DesiredSource::Coord, TICK_MS);
        assert_eq!(r1.releases, vec![(TICK_MS, ReleaseReason::OutOfRange)]);
        assert_eq!(t1.walk, WalkVerdict::AllHeld, "falls through to the walk");
        assert!(coord.live_owner(&key(1), TICK_MS).is_none(), "slot 1 freed");
        assert_eq!(r0.held.as_ref().map(|h| h.slot), Some(0));

        // A local-registry fallback (a failed desired-state read) never does.
        let mut r2 = SimRunner::new("r2", Liveness::Live, SpawnBehavior::Ok).holding(1, 0);
        coord
            .owners
            .insert(key(1), ("r2".into(), 2 * TICK_MS + TTL_MS));
        sim_tick(
            &mut coord,
            &mut r2,
            1,
            DesiredSource::LocalRegistry,
            2 * TICK_MS,
        );
        assert!(r2.releases.is_empty() && r2.held.is_some());
    }
}
