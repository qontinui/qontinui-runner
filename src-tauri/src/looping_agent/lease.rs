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
//! ```
//!
//! A dead runner stops heartbeating, its lease expires on TTL, and the freed
//! slot is acquired by exactly ONE surviving supervisor — one respawn, not a
//! storm, because the atomic acquire admits exactly one winner.

use super::policy::Action;

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
    /// The key vanished (TTL expired) or was taken — we do NOT hold this slot.
    Stolen,
    /// Transport error. Says nothing about ownership.
    Unavailable(String),
}

/// The slot lease this supervisor currently holds for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSlot {
    pub slot: u32,
    pub resource_key: String,
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
/// * `Stolen` is COORD'S AUTHORITATIVE ANSWER that we do not hold it → drop it.
/// * `Unavailable` is OUR failure to ask → keep believing we hold it. Dropping
///   on a transport blip would make every runner release its slot during a coord
///   hiccup and then all race to re-acquire — self-inflicted churn. If coord is
///   genuinely gone, our TTL expires anyway and the slot frees itself, which is
///   the same outcome via the mechanism that is actually authoritative.
pub fn still_held_after_heartbeat(outcome: &HeartbeatOutcome) -> bool {
    match outcome {
        HeartbeatOutcome::Ok => true,
        HeartbeatOutcome::Stolen => false,
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
}
