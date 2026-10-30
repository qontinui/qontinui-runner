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
//! acquire(role:<tenant>:<role>:<slot>)  ──granted/claimed/──> spawn permitted
//!                                         renewed
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

/// Who holds a slot lease that is not ours, as coord named it.
///
/// `machine_id` is the holder's runner machine id — the key coord stores the
/// claim under, and the stable identifier fleet views (`coord_query_workers`)
/// use. `session` is the holder's `agent_session_id` when the holding token is
/// session-scoped (`machine:session`); our own token is bare-machine, so a
/// `Some` session on our own machine id means a DIFFERENT session on this box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHolder {
    pub machine_id: String,
    pub session: Option<String>,
}

/// Outcome of an acquire attempt, folded to the three cases the supervisor can
/// act on.
///
/// Two discriminator vocabularies reach this fold. The slot lease uses
/// `kind = semantic_resource` with no topic, and coord's `claims_acquire`
/// diverts exactly that request shape to the semantic-reserve door
/// (`routes.rs:7100` → `semantic_reserve_response`), so the LIVE wire is
/// `ReserveOutcome`: `granted` | `renewed` | `held` (holder in `holder`). The
/// plain `AcquireResult` vocabulary — `claimed` | `renewed` | `held` (holder in
/// `current_holder`) — is tolerated too, in case coord ever stops diverting.
/// See [`parse_acquire_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// `granted` (reserve door, fresh), `claimed` (plain door, fresh) or
    /// `renewed` (we already held it — an idempotent re-acquire that extends
    /// the TTL). All three mean: this slot is OURS.
    Acquired,
    /// `held` — a DIFFERENT owner holds this slot. Another live shepherd is
    /// filling it; do nothing at all. `holder` is `None` when coord's body named
    /// nobody (key absent or an empty string) — never a fabricated id.
    Held { holder: Option<SlotHolder> },
    /// Transport error, non-2xx, or an unparseable body. NOT permission.
    Unavailable(String),
}

/// Outcome of a heartbeat on a lease we believe we hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// Still ours; TTL extended.
    Ok,
    /// Another owner holds this slot now — coord returned a current holder
    /// (`None` when the body named nobody).
    Stolen { holder: Option<SlotHolder> },
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
        HeartbeatOutcome::Stolen { .. } => false,
        HeartbeatOutcome::NotHeld => false,
        HeartbeatOutcome::Unavailable(_) => true,
    }
}

/// The first non-empty string among `keys` on `v`.
///
/// An empty string counts as absent: coord emits `""` when the key vanished
/// between SET-NX and GET (`claims.rs:527-531`), and the reserve mapper emits
/// `held` + `""` for any non-claim acquire variant (`semantic_reserve.rs:398-405`).
fn first_non_empty_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|k| v.get(*k).and_then(|s| s.as_str()))
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// The holder named by an acquire/heartbeat body.
///
/// Precedence: `holder` (the semantic-reserve shape — today's live wire), then
/// `current_holder` (the plain `AcquireResult` / `HeartbeatResult::Stolen`
/// shape). The session is read from `current_holder_session` on both shapes,
/// and only once a machine id was found — a session with no machine is not a
/// holder.
fn holder_from_body(v: &serde_json::Value) -> Option<SlotHolder> {
    let machine_id = first_non_empty_str(v, &["holder", "current_holder"])?;
    let session = first_non_empty_str(v, &["current_holder_session"]);
    Some(SlotHolder {
        machine_id,
        session,
    })
}

/// Parse a decoded `/claims/acquire` body (either door's shape — see
/// [`AcquireOutcome`]).
pub fn parse_acquire_body(v: &serde_json::Value) -> AcquireOutcome {
    let Some(result) = v.get("result").and_then(|r| r.as_str()) else {
        return AcquireOutcome::Unavailable("body missing `result` discriminator".to_string());
    };
    match result {
        // `granted` is the reserve door's fresh-grant word; `claimed` the plain
        // door's. Both are coord's authoritative grant to OUR owner token —
        // mapping `granted` is not fail-open, it is the same thing `claimed`
        // means. Leaving it unmapped stranded every fresh grant for one tick.
        "claimed" | "granted" | "renewed" => AcquireOutcome::Acquired,
        "held" => AcquireOutcome::Held {
            holder: holder_from_body(v),
        },
        // Topic/tenant/invalid results can't arise for this call shape (no
        // topic, no explicit tenant), but an unknown discriminator is NEVER
        // silently treated as permission.
        other => AcquireOutcome::Unavailable(format!("unexpected acquire result `{other}`")),
    }
}

/// Parse a decoded `/claims/heartbeat` body. The heartbeat is NOT diverted to
/// the reserve door, so `stolen` carries the plain `current_holder` /
/// `current_holder_session` (`claims.rs:616-651`); the same precedence as
/// [`parse_acquire_body`] applies so a future divert parses too.
pub fn parse_heartbeat_body(v: &serde_json::Value) -> HeartbeatOutcome {
    let Some(result) = v.get("result").and_then(|r| r.as_str()) else {
        return HeartbeatOutcome::Unavailable("body missing `result` discriminator".to_string());
    };
    match result {
        "ok" => HeartbeatOutcome::Ok,
        "stolen" => HeartbeatOutcome::Stolen {
            holder: holder_from_body(v),
        },
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

    fn holder(machine_id: &str, session: Option<&str>) -> Option<SlotHolder> {
        Some(SlotHolder {
            machine_id: machine_id.to_string(),
            session: session.map(str::to_string),
        })
    }

    fn acquire(body: &str) -> AcquireOutcome {
        parse_acquire_body(&serde_json::from_str(body).expect("fixture is valid JSON"))
    }

    fn heartbeat(body: &str) -> HeartbeatOutcome {
        parse_heartbeat_body(&serde_json::from_str(body).expect("fixture is valid JSON"))
    }

    #[test]
    fn only_an_acquired_outcome_grants_permission() {
        assert!(spawn_permitted(&AcquireOutcome::Acquired));
        assert!(!spawn_permitted(&AcquireOutcome::Held {
            holder: holder("other-machine", None)
        }));
        assert!(!spawn_permitted(&AcquireOutcome::Held { holder: None }));
        // "I couldn't reach coord" is NOT "the slot is free" — this assertion
        // is the storm-prevention invariant in one line.
        assert!(!spawn_permitted(&AcquireOutcome::Unavailable(
            "connection refused".into()
        )));
    }

    #[test]
    fn held_means_another_live_shepherd_owns_the_slot_so_do_nothing() {
        let outcome = acquire(r#"{"result":"held","current_holder":"machine-b"}"#);
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                holder: holder("machine-b", None)
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

    // -- parse_acquire_body: literal bodies in coord's serde shapes ----------
    //
    // Reserve shape = `ReserveOutcome` (`semantic_reserve.rs:263-345`,
    // internally tagged on `result`, augmented with kind/resource_key/class/
    // name/grammar). Plain shape = `AcquireResult` (`claims.rs:502-560`).

    /// The LIVE wire: the reserve door's `held` names the holder in `holder`.
    /// A mutation dropping the `"holder"` lookup turns this red.
    #[test]
    fn reserve_held_with_holder_and_session_names_both() {
        let outcome = acquire(
            r#"{"result":"held","holder":"3e7e4b04-75de-4efb-b718-c8ce8fcf7b17","current_holder_session":"5d7c3a7e-0b8b-4f3e-9d43-1c2a7e9b0f11","kind":"semantic_resource","resource_key":"role:c231d9da-0ca8-4fe4-bd81-0e3d6c20339a:merge_shepherd:0","class":"role","name":"c231d9da-0ca8-4fe4-bd81-0e3d6c20339a:merge_shepherd:0","grammar":"class_name"}"#,
        );
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                holder: holder(
                    "3e7e4b04-75de-4efb-b718-c8ce8fcf7b17",
                    Some("5d7c3a7e-0b8b-4f3e-9d43-1c2a7e9b0f11")
                )
            }
        );
        assert!(!spawn_permitted(&outcome));
    }

    #[test]
    fn reserve_held_without_session_key_has_no_session() {
        let outcome = acquire(
            r#"{"result":"held","holder":"3e7e4b04-75de-4efb-b718-c8ce8fcf7b17","kind":"semantic_resource","resource_key":"role:t:merge_shepherd:0"}"#,
        );
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                holder: holder("3e7e4b04-75de-4efb-b718-c8ce8fcf7b17", None)
            }
        );
        assert!(!spawn_permitted(&outcome));
    }

    #[test]
    fn legacy_plain_held_reads_current_holder_and_session() {
        let outcome = acquire(
            r#"{"result":"held","current_holder":"machine-legacy","current_holder_session":"sess-legacy"}"#,
        );
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                holder: holder("machine-legacy", Some("sess-legacy"))
            }
        );
        assert!(!spawn_permitted(&outcome));
    }

    #[test]
    fn when_both_keys_are_present_holder_wins() {
        let outcome = acquire(
            r#"{"result":"held","holder":"machine-reserve","current_holder":"machine-plain"}"#,
        );
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                holder: holder("machine-reserve", None)
            }
        );
        assert!(!spawn_permitted(&outcome));
    }

    #[test]
    fn held_with_no_holder_key_is_held_with_unknown_holder() {
        let outcome = acquire(r#"{"result":"held","kind":"semantic_resource"}"#);
        assert_eq!(outcome, AcquireOutcome::Held { holder: None });
        assert!(
            !spawn_permitted(&outcome),
            "an unnamed holder is still held"
        );
    }

    #[test]
    fn held_with_an_empty_holder_is_never_some_empty_string() {
        // The vanish race / non-claim reserve variant: `held` + `""`. A session
        // with no machine id is not a holder either.
        let outcome = acquire(r#"{"result":"held","holder":"","current_holder_session":"s"}"#);
        assert_eq!(outcome, AcquireOutcome::Held { holder: None });
        assert!(!spawn_permitted(&outcome));

        // An empty `holder` falls through to a non-empty legacy key.
        let outcome = acquire(r#"{"result":"held","holder":"","current_holder":"machine-c"}"#);
        assert_eq!(
            outcome,
            AcquireOutcome::Held {
                holder: holder("machine-c", None)
            }
        );
        assert!(!spawn_permitted(&outcome));
    }

    /// The reserve door's fresh-grant word. A mutation dropping `"granted"`
    /// turns this red.
    #[test]
    fn reserve_granted_is_acquired() {
        let outcome = acquire(
            r#"{"result":"granted","ttl_seconds":120,"kind":"semantic_resource","resource_key":"role:t:merge_shepherd:0","class":"role","name":"t:merge_shepherd:0"}"#,
        );
        assert_eq!(outcome, AcquireOutcome::Acquired);
        assert!(spawn_permitted(&outcome));
    }

    #[test]
    fn plain_claimed_and_renewed_are_acquired() {
        assert_eq!(
            acquire(r#"{"result":"claimed","ttl_seconds":120}"#),
            AcquireOutcome::Acquired
        );
        // `renewed` = we already hold it (restart / re-acquire) — still ours.
        assert_eq!(
            acquire(r#"{"result":"renewed","ttl_seconds":120}"#),
            AcquireOutcome::Acquired
        );
    }

    #[test]
    fn acquire_unknown_empty_or_missing_result_is_unavailable_never_permission() {
        for body in [
            r#"{"result":"tenant_not_bound"}"#,
            r#"{"result":""}"#,
            r#"{"ttl_seconds":120}"#,
            r#"{"result":7}"#,
            r#"null"#,
        ] {
            let outcome = acquire(body);
            assert!(
                matches!(outcome, AcquireOutcome::Unavailable(_)),
                "{body} must be Unavailable, got {outcome:?}"
            );
            assert!(
                !spawn_permitted(&outcome),
                "{body} must never be permission"
            );
        }
        assert_eq!(
            acquire(r#"{"ttl_seconds":120}"#),
            AcquireOutcome::Unavailable("body missing `result` discriminator".into())
        );
        assert_eq!(
            acquire(r#"{"result":"tenant_not_bound"}"#),
            AcquireOutcome::Unavailable("unexpected acquire result `tenant_not_bound`".into())
        );
    }

    // -- parse_heartbeat_body: plain `HeartbeatResult` (`claims.rs:616-651`) --

    #[test]
    fn heartbeat_stolen_names_the_new_holder() {
        let outcome = heartbeat(
            r#"{"result":"stolen","current_holder":"3e7e4b04-75de-4efb-b718-c8ce8fcf7b17","current_holder_session":"sess-x"}"#,
        );
        assert_eq!(
            outcome,
            HeartbeatOutcome::Stolen {
                holder: holder("3e7e4b04-75de-4efb-b718-c8ce8fcf7b17", Some("sess-x"))
            }
        );
        assert!(!still_held_after_heartbeat(&outcome));
    }

    #[test]
    fn heartbeat_stolen_without_a_holder_is_stolen_with_none() {
        let outcome = heartbeat(r#"{"result":"stolen"}"#);
        assert_eq!(outcome, HeartbeatOutcome::Stolen { holder: None });
        assert!(!still_held_after_heartbeat(&outcome));
        assert_eq!(
            heartbeat(r#"{"result":"stolen","current_holder":null}"#),
            HeartbeatOutcome::Stolen { holder: None }
        );
    }

    #[test]
    fn heartbeat_ok_keeps_the_lease_and_a_transport_error_does_too() {
        assert_eq!(
            heartbeat(r#"{"result":"ok","ttl_seconds":120}"#),
            HeartbeatOutcome::Ok
        );
        assert!(still_held_after_heartbeat(&heartbeat(r#"{"result":"ok"}"#)));
        assert!(
            !still_held_after_heartbeat(&heartbeat(r#"{"result":"stolen"}"#)),
            "stolen is coord's authoritative answer that we do not hold it"
        );
        assert!(
            still_held_after_heartbeat(&HeartbeatOutcome::Unavailable("timeout".into())),
            "a transport blip must not make every runner drop and re-race its slot"
        );
        assert!(matches!(
            heartbeat(r#"{"ttl_seconds":120}"#),
            HeartbeatOutcome::Unavailable(_)
        ));
        assert!(matches!(
            heartbeat(r#"{"result":"wat"}"#),
            HeartbeatOutcome::Unavailable(_)
        ));
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
        let outcome = heartbeat(r#"{"result":"not_held"}"#);
        assert_eq!(outcome, HeartbeatOutcome::NotHeld);
        assert!(
            !still_held_after_heartbeat(&outcome),
            "an expired claim is not held, however politely coord says so"
        );
        assert!(
            !matches!(outcome, HeartbeatOutcome::Stolen { .. }),
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
                        holder: Some(SlotHolder {
                            machine_id: holder.clone(),
                            session: None,
                        }),
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
