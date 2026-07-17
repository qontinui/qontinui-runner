//! Coord desired-state reconciliation — the PURE resolver.
//!
//! Phase 8 of plan `2026-07-17-session-autonomy-fabric.md`. Phase 1's Tier-0
//! supervisor decided whether to keep a looping agent alive from ONE input: the
//! runner-local registry flag. That is correct for a single machine and wrong
//! for a fleet — five runners each reading their own local registry will each
//! keep "the" merge shepherd alive, and the tenant gets five shepherds.
//!
//! Phase 8 makes COORD the declared source of truth ("this tenant wants N merge
//! shepherds, armed") and leaves the local registry as the fallback. This module
//! is the pure fold of those two inputs into one answer; the impure poll lives
//! in the bin at `looping_agent_coord.rs`, and the actual mutual exclusion is
//! enforced by the slot leases in [`super::lease`] — this resolver only decides
//! WHETHER and HOW MANY, never WHO.
//!
//! ## Fail-safe contract (the important part)
//!
//! Coord `armed = false`, no rule for the role, or coord UNREACHABLE all fall
//! back to the local registry posture — whose seed is `enabled = false`
//! (`registry.rs:161-162`). The direction of that fallback is deliberate and
//! load-bearing: an unreachable coord must NEVER be read as permission to
//! spawn. The failure mode we accept (a coord outage stops new shepherds from
//! being spawned) is strictly safer than the one we refuse (a coord outage, or
//! a half-parsed response, spawns agents nobody asked for).

/// One coord-declared desired-state rule, as served by
/// `GET /coord/agent-desired-state`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordDesiredState {
    /// The `coord.sessions.role` wire value this rule governs (`merge_shepherd`).
    pub role: String,
    /// How many sessions of this role the tenant wants running FLEET-WIDE —
    /// NOT per runner. Every runner sees the same N and they race for the N
    /// slot leases, which is what turns "N" into a fleet-wide invariant.
    pub count: u32,
    /// The operator-held arming switch.
    pub armed: bool,
}

/// Which input decided the effective posture. Carried purely so the supervisor
/// can log an edge-triggered transition ("now following coord" / "fell back to
/// the local registry") instead of leaving an operator guessing why a shepherd
/// did or didn't come up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredSource {
    /// An armed coord rule decided this.
    Coord,
    /// No armed coord rule (absent, disarmed, or coord unreachable) — the
    /// runner-local registry decided.
    LocalRegistry,
}

/// The resolved answer for one agent this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveDesired {
    /// Should this agent be running at all?
    pub running: bool,
    /// How many fleet-wide slots exist for its role. The supervisor races for
    /// a free one; holding none means it does not spawn.
    pub slots: u32,
    pub source: DesiredSource,
}

/// Hard ceiling on slots the runner will ever chase, mirroring coord's
/// `agent_desired_state::MAX_COUNT`.
///
/// Duplicated deliberately rather than shared: coord and the runner ship as
/// separate binaries on independent release trains, so a runner in the field
/// can be older than the coord serving it. The storm cap must hold even when a
/// coord (or a hand-edited rule row, or a future coord that raises its own
/// ceiling) hands this runner a larger number. Clamping locally means the
/// blast radius of any such disagreement is bounded by the code actually doing
/// the spawning.
pub const MAX_SLOTS: u32 = 16;

/// Fold coord's declaration and the local registry into the effective posture.
///
/// * `local_enabled` — `def.enabled` (the operator's local master switch).
/// * `local_running` — `def.desired_state == DesiredState::Running`.
/// * `coord` — the rule for THIS agent's role, or `None` when there is no rule
///   or coord could not be reached.
///
/// An armed coord rule with `count == 0` is a real answer, not a missing one:
/// it means "the tenant wants zero of these right now" and it WINS over a
/// locally-enabled registry. That is what makes coord a usable fleet-wide OFF
/// switch — an operator disabling shepherds centrally must not be silently
/// overridden by a stale local flag on one runner.
pub fn resolve(
    local_enabled: bool,
    local_running: bool,
    coord: Option<&CoordDesiredState>,
) -> EffectiveDesired {
    match coord {
        Some(rule) if rule.armed => {
            let slots = rule.count.min(MAX_SLOTS);
            EffectiveDesired {
                running: slots > 0,
                slots,
                source: DesiredSource::Coord,
            }
        }
        // Disarmed / absent / unreachable → the local registry decides, and its
        // seed posture is `enabled = false`. One slot: a runner acting on its
        // own local flag is a single-machine actor by definition.
        _ => EffectiveDesired {
            running: local_enabled && local_running,
            slots: 1,
            source: DesiredSource::LocalRegistry,
        },
    }
}

/// Pick this agent's rule out of the tenant's rule set by role.
pub fn rule_for_role<'a>(
    rules: &'a [CoordDesiredState],
    role: &str,
) -> Option<&'a CoordDesiredState> {
    rules.iter().find(|r| r.role == role)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coord(role: &str, count: u32, armed: bool) -> CoordDesiredState {
        CoordDesiredState {
            role: role.to_string(),
            count,
            armed,
        }
    }

    #[test]
    fn an_armed_coord_rule_decides_and_carries_its_slot_count() {
        let r = resolve(false, false, Some(&coord("merge_shepherd", 3, true)));
        assert_eq!(
            r,
            EffectiveDesired {
                running: true,
                slots: 3,
                source: DesiredSource::Coord,
            },
            "an armed coord rule must run the agent even though the local registry is disabled"
        );
    }

    #[test]
    fn coord_unreachable_falls_back_to_the_local_registry() {
        // THE fail-safe: coord is unreachable (None) and the local seed posture
        // is disabled → nothing runs. An unreachable coord is never permission
        // to spawn.
        let r = resolve(false, false, None);
        assert!(
            !r.running,
            "an unreachable coord must NEVER be read as permission to spawn"
        );
        assert_eq!(r.source, DesiredSource::LocalRegistry);

        // ...and a locally-enabled agent still runs while coord is unreachable,
        // so a coord outage cannot strand an operator who armed it locally.
        let r = resolve(true, true, None);
        assert!(r.running);
        assert_eq!(r.slots, 1, "a local-registry actor holds a single slot");
        assert_eq!(r.source, DesiredSource::LocalRegistry);
    }

    #[test]
    fn a_disarmed_coord_rule_falls_back_to_the_local_registry() {
        let r = resolve(false, false, Some(&coord("merge_shepherd", 5, false)));
        assert!(!r.running, "disarmed + locally disabled = nothing runs");
        assert_eq!(r.source, DesiredSource::LocalRegistry);

        // Disarmed does NOT mean "force off" — it means "coord abstains", so a
        // locally-enabled agent keeps running.
        let r = resolve(true, true, Some(&coord("merge_shepherd", 5, false)));
        assert!(r.running);
        assert_eq!(r.slots, 1);
        assert_eq!(r.source, DesiredSource::LocalRegistry);
    }

    #[test]
    fn an_armed_zero_count_is_a_fleet_wide_off_switch_that_beats_a_local_flag() {
        // The lever an operator pulls to stop the fleet. A stale local
        // `enabled=true` on one runner must not resurrect a shepherd the tenant
        // centrally turned off.
        let r = resolve(true, true, Some(&coord("merge_shepherd", 0, true)));
        assert!(
            !r.running,
            "an armed count=0 must override a locally-enabled registry"
        );
        assert_eq!(r.slots, 0);
        assert_eq!(r.source, DesiredSource::Coord);
    }

    #[test]
    fn a_local_stop_is_honored_when_coord_abstains() {
        // desired_state=Stopped locally, coord silent → stopped.
        assert!(!resolve(true, false, None).running);
    }

    #[test]
    fn slots_are_clamped_to_the_storm_ceiling() {
        // Defense in depth against an older runner meeting a newer coord (or a
        // hand-edited rule): the runner that does the spawning caps itself.
        let r = resolve(false, false, Some(&coord("merge_shepherd", 9000, true)));
        assert_eq!(r.slots, MAX_SLOTS);
        assert!(r.running);
    }

    #[test]
    fn rule_lookup_is_by_role() {
        let rules = vec![
            coord("merge_fixer", 1, true),
            coord("merge_shepherd", 2, true),
        ];
        assert_eq!(rule_for_role(&rules, "merge_shepherd").unwrap().count, 2);
        assert_eq!(rule_for_role(&rules, "merge_fixer").unwrap().count, 1);
        assert!(
            rule_for_role(&rules, "gardener").is_none(),
            "an unknown role must resolve to no rule (→ local fallback), not a wrong rule"
        );
    }

    #[test]
    fn another_roles_rule_never_leaks_into_this_agents_posture() {
        // A tenant running an armed fixer must not thereby spawn shepherds.
        let rules = vec![coord("merge_fixer", 4, true)];
        let mine = rule_for_role(&rules, "merge_shepherd");
        let r = resolve(false, false, mine);
        assert!(!r.running);
        assert_eq!(r.source, DesiredSource::LocalRegistry);
    }
}
