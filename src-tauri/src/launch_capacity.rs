//! The launch capacity this device publishes: how many more unattended launches
//! the runner's own admission would take RIGHT NOW, and the verdict behind it.
//!
//! Plan `2026-11-14-coord-spawn-admission-uses-the-runners-own-launch-ceiling`
//! Phase 2 (D1). coord places autofix spawns; the runner then decides whether to
//! launch them ([`crate::agent_runtime`]'s `admit_launch`). The two decisions
//! used different numbers — coord a fleet-wide 400-thread line against the raw
//! thread count, the runner its per-machine WARN ceiling against the graded
//! count — so coord kept placing spawns the device refused. The fix is that
//! the device publishes the answer its own admission computes, and coord admits
//! against that instead of re-deriving a fold that changes with every runner PR.
//!
//! ## One computation, three readers
//!
//! [`launch_headroom`] is the ONLY place the published numbers are computed,
//! and it is computed through the same projection `admit_launch` decides on
//! ([`crate::resource_guard::projected_verdict`], which is
//! [`crate::resource_guard::AdmissionReading::projected`]'s reported half). Its
//! three readers are the launch gate's equivalence (headroom 0 ⇔ the next
//! launch is refused, pinned by
//! `launch_headroom_is_zero_exactly_when_admit_launch_refuses`), the `/health`
//! `spawnAdmission` block, and the host-lane resource sample coord ingests.
//!
//! ## What the headroom counts
//!
//! The smallest `k` in `0..=cap` for which charging `reserved + k` projected
//! spawns against the readings trips ANY lane (a warn is enough — that is what
//! `admit_launch` refuses on), or `cap` when none does. It is deliberately
//! stricter than `admit_launch` itself, which charges no projected unit per
//! LAUNCH: each further launch coord places is charged one unit here, so `k`
//! launches placed back to back are each still admitted when the readings move
//! as projected.
//!
//! ## The verdict vocabulary — a wire value
//!
//! `proceed` / `warn` / `critical` (the composed projected verdict at `k = 0`,
//! i.e. what the next launch would get), `guard_disabled` (the session guard is
//! off: no reading is taken and nothing is enforced — headroom is UNKNOWN, never
//! 0 and never the cap), and `unreadable` (one lane's sensor gave no reading:
//! that lane fails open exactly as the gate does, so the headroom is computed
//! from the readable lane alone). An `unreadable` verdict deliberately HIDES
//! the readable lane's own severity — a box whose thread table is unreadable
//! and whose memory lane warns publishes `unreadable`, not `warn` — and no
//! second field carries it: coord admits on `launch_headroom` alone (0 when
//! the readable lane already trips), so the verdict is display and forensics,
//! never the admission input. coord's
//! `coord.device_resource_samples.launch_verdict` CHECK constraint carries the
//! same five words, so renaming one here makes every sample this runner
//! publishes fail ingest.

use crate::resource_guard::{projected_verdict, LaneVerdicts, ProjectedSpawnCost};

/// The ceiling on the published headroom. Enough to say "plenty of room" on the
/// largest box this fleet runs without 1,000 projections per sample; coord
/// treats any positive number as room, so a capped value loses nothing it reads.
pub(crate) const LAUNCH_HEADROOM_CAP: usize = 64;

/// The published launch verdict. See the module doc for the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchVerdict {
    Proceed,
    Warn,
    Critical,
    GuardDisabled,
    Unreadable,
}

impl LaunchVerdict {
    /// The wire word — coord's `launch_verdict` CHECK constraint vocabulary.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            LaunchVerdict::Proceed => "proceed",
            LaunchVerdict::Warn => "warn",
            LaunchVerdict::Critical => "critical",
            LaunchVerdict::GuardDisabled => "guard_disabled",
            LaunchVerdict::Unreadable => "unreadable",
        }
    }
}

/// What this device would admit right now, and the thread-lane inputs behind it
/// (for display and forensics; coord admits on `headroom` alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaunchCapacity {
    pub(crate) verdict: LaunchVerdict,
    /// Further projected launches admitted before one is refused. `None` only
    /// for [`LaunchVerdict::GuardDisabled`] — a disabled guard enforces nothing,
    /// so there is no number to publish, and a `0` would read as "full".
    pub(crate) headroom: Option<usize>,
    /// The GRADED thread reading the thread lane judged. `None` when the guard
    /// is disabled or the thread table could not be read.
    pub(crate) graded: Option<usize>,
    /// The effective thread ceilings the reading was judged against. `None`
    /// only when the guard is disabled.
    pub(crate) warn: Option<usize>,
    pub(crate) critical: Option<usize>,
}

/// PURE: the launch capacity over one admission reading. See the module doc.
///
/// - `lanes: None` is a disabled session guard ⇒ [`LaunchVerdict::GuardDisabled`]
///   with every number `None`.
/// - `reserved` is the count of admitted, not-yet-running continuations — the
///   same charge `admit_launch` applies — so `headroom == 0` exactly when
///   `admit_launch` would refuse the next launch.
pub(crate) fn launch_headroom(
    lanes: Option<&LaneVerdicts>,
    reserved: usize,
    cost: ProjectedSpawnCost,
    cap: usize,
) -> LaunchCapacity {
    let Some(readings) = lanes else {
        return LaunchCapacity {
            verdict: LaunchVerdict::GuardDisabled,
            headroom: None,
            graded: None,
            warn: None,
            critical: None,
        };
    };
    let trips = |k: usize| {
        projected_verdict(lanes, reserved.saturating_add(k), cost)
            .tripped()
            .is_some()
    };
    let headroom = (0..=cap).find(|&k| trips(k)).unwrap_or(cap);
    let unreadable = readings.free_commit_bytes.is_none() || readings.threads.graded.is_none();
    let verdict = if unreadable {
        LaunchVerdict::Unreadable
    } else {
        match projected_verdict(lanes, reserved, cost).tripped() {
            None => LaunchVerdict::Proceed,
            Some(("critical", _)) => LaunchVerdict::Critical,
            Some(_) => LaunchVerdict::Warn,
        }
    };
    LaunchCapacity {
        verdict,
        headroom: Some(headroom),
        graded: readings.threads.graded,
        warn: Some(readings.threads.warn_ceiling),
        critical: Some(readings.threads.critical_ceiling),
    }
}

/// [`launch_headroom`] over a whole [`crate::resource_guard::AdmissionReading`]
/// at the published cap — the form the sample publisher and `/health` take.
pub(crate) fn from_reading(
    reading: &crate::resource_guard::AdmissionReading,
    reserved: usize,
) -> LaunchCapacity {
    launch_headroom(
        reading.lanes.as_ref(),
        reserved,
        reading.cost,
        LAUNCH_HEADROOM_CAP,
    )
}

/// The live launch capacity over a memory reading the caller already took —
/// the same composition [`crate::resource_guard::AdmissionReading::live`]
/// makes (both lanes plus the live projected cost), charged with the
/// continuations admitted and not yet running. The host-lane sampler passes the
/// `memory_status()` it publishes, so the sample's memory figures and its
/// launch capacity are one reading. The thread reading is still a synchronous
/// OS read — call it from a blocking context (the sampler runs inside
/// `spawn_blocking_tracked`), never on an async worker.
pub(crate) fn live_with_memory(memory: (&'static str, Option<u64>, Option<u64>)) -> LaunchCapacity {
    let reading = crate::resource_guard::AdmissionReading {
        lanes: crate::resource_guard::lane_verdicts_with_memory(memory),
        cost: crate::resource_guard::ProjectedSpawnCost::live(),
    };
    from_reading(&reading, crate::agent_runtime::reserved_spawns_now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource_guard::{
        evaluate, evaluate_threads, AdmissionReading, SpawnGate, ThreadCeilings, ThreadLane,
    };
    use crate::settings::SessionGuardSettings;

    const WARN_FLOOR: u64 = 3_000;
    const CRITICAL_FLOOR: u64 = 1_000;

    fn lanes(free: Option<u64>, graded: Option<usize>, warn: usize, critical: usize) -> LaneVerdicts {
        let ceilings = ThreadCeilings { warn, critical };
        let floors = SessionGuardSettings {
            warn_free_commit_bytes: WARN_FLOOR,
            critical_free_commit_bytes: CRITICAL_FLOOR,
            ..SessionGuardSettings::default()
        };
        LaneVerdicts {
            memory_lane: "host",
            free_commit_bytes: free,
            free_phys_bytes: None,
            warn_free_commit_bytes: WARN_FLOOR,
            critical_free_commit_bytes: CRITICAL_FLOOR,
            memory: evaluate("host", free, &floors),
            threads: ThreadLane {
                graded,
                warn_ceiling: warn,
                critical_ceiling: critical,
                verdict: evaluate_threads(graded, true, ceilings),
            },
        }
    }

    fn cost(commit_bytes: u64, threads: usize) -> ProjectedSpawnCost {
        ProjectedSpawnCost {
            commit_bytes,
            threads,
        }
    }

    /// THE equivalence the publication rests on: coord admits a device only on
    /// `headroom > 0`, so a device publishing room it would refuse — or no room
    /// it would grant — re-opens the very loop this plan closes.
    ///
    /// Mutation: in [`launch_headroom`], count only a CRITICAL projection as a
    /// trip (`tripped().is_some()` → `matches!(.., SpawnGate::Critical(_))`):
    /// the warn-band rows publish headroom > 0 while `admit_launch` refuses.
    #[test]
    fn launch_headroom_is_zero_exactly_when_admit_launch_refuses() {
        const WARN: usize = 376;
        const CRITICAL: usize = 504;
        // Memory in band (between the floors, so the memory lane warns) and out
        // of band (plenty free).
        for free in [2_000_u64, 1_000_000] {
            for graded in [WARN - 3, WARN - 1, WARN, WARN + 1] {
                for reserved in [0_usize, 2] {
                    let reading = AdmissionReading {
                        lanes: Some(lanes(Some(free), Some(graded), WARN, CRITICAL)),
                        cost: cost(100, 2),
                    };
                    let refused =
                        crate::agent_runtime::launch_admission(&reading, reserved).0.is_err();
                    let capacity = from_reading(&reading, reserved);
                    assert_eq!(
                        capacity.headroom == Some(0),
                        refused,
                        "free {free}, graded {graded}, reserved {reserved}: \
                         headroom {:?} vs admit_launch refused={refused}",
                        capacity.headroom
                    );
                    // And the published verdict is the one admit_launch acts on.
                    assert_eq!(
                        capacity.verdict == LaunchVerdict::Proceed,
                        !refused,
                        "free {free}, graded {graded}, reserved {reserved}: {:?}",
                        capacity.verdict
                    );
                }
            }
        }
    }

    /// warn 376, graded 370, 2 threads per launch: k=3 projects 376 (AT the
    /// ceiling, not over it — `evaluate_threads` trips strictly above), k=4
    /// projects 378 and trips. So 4 more fit.
    ///
    /// Mutation: `>` → `>=` in `evaluate_threads`'s warn arm gives 3.
    #[test]
    fn launch_headroom_counts_how_many_more_fit() {
        let l = lanes(Some(u64::MAX / 2), Some(370), 376, 504);
        let capacity = launch_headroom(Some(&l), 0, cost(0, 2), LAUNCH_HEADROOM_CAP);
        assert_eq!(capacity.headroom, Some(4));
        assert_eq!(capacity.verdict, LaunchVerdict::Proceed);
        assert_eq!(
            (capacity.graded, capacity.warn, capacity.critical),
            (Some(370), Some(376), Some(504))
        );
        // Reserved continuations debit the same count.
        let debited = launch_headroom(Some(&l), 1, cost(0, 2), LAUNCH_HEADROOM_CAP);
        assert_eq!(debited.headroom, Some(3));
    }

    /// A disabled guard enforces nothing: the published answer is a word, not
    /// a number — `0` would read as "full" and the cap as "measured empty".
    ///
    /// Mutation: return `headroom: Some(0)` (or `Some(cap)`) for `None` lanes.
    #[test]
    fn a_disabled_guard_publishes_guard_disabled_not_zero() {
        let capacity = launch_headroom(None, 5, cost(100, 2), LAUNCH_HEADROOM_CAP);
        assert_eq!(capacity.verdict, LaunchVerdict::GuardDisabled);
        assert_eq!(capacity.verdict.as_str(), "guard_disabled");
        assert_eq!(capacity.headroom, None);
        assert_eq!(
            (capacity.graded, capacity.warn, capacity.critical),
            (None, None, None)
        );
    }

    /// One unreadable lane fails open exactly as the gate does, so the
    /// headroom comes from the readable lane — but the verdict says the reading
    /// was partial, never a confident `proceed`.
    ///
    /// Mutation: drop the `unreadable` arm (the verdict reads `proceed`).
    #[test]
    fn an_unreadable_lane_publishes_unreadable_not_proceed() {
        // Thread table unreadable; memory readable against the 3_000 warn
        // floor at 100 bytes a launch: k=5 projects 3_050 (no trip), k=6
        // projects 2_950 and trips, so 6 more fit.
        let no_threads = lanes(Some(3_550), None, 376, 504);
        let capacity = launch_headroom(Some(&no_threads), 0, cost(100, 2), LAUNCH_HEADROOM_CAP);
        assert_eq!(capacity.verdict, LaunchVerdict::Unreadable);
        assert_eq!(capacity.verdict.as_str(), "unreadable");
        assert_eq!(capacity.headroom, Some(6), "computed from the memory lane");
        assert_eq!(capacity.graded, None);

        // Memory unreadable; threads readable: 370 graded, warn 376, 2 each.
        let no_memory = lanes(None, Some(370), 376, 504);
        let capacity = launch_headroom(Some(&no_memory), 0, cost(100, 2), LAUNCH_HEADROOM_CAP);
        assert_eq!(capacity.verdict, LaunchVerdict::Unreadable);
        assert_eq!(capacity.headroom, Some(4), "computed from the thread lane");
        assert_eq!(capacity.graded, Some(370));
    }

    /// A box with endless room publishes the cap, not an unbounded count, and
    /// a trip at exactly the cap still reads as the cap.
    ///
    /// Mutation: `(0..=cap)` → `(0..)` with no bound (the call never ends) or
    /// `unwrap_or(cap)` → `unwrap_or(usize::MAX)`.
    #[test]
    fn headroom_is_capped_at_64() {
        assert_eq!(LAUNCH_HEADROOM_CAP, 64);
        let roomy = lanes(Some(u64::MAX / 2), Some(1), 100_000, 200_000);
        let capacity = launch_headroom(Some(&roomy), 0, cost(1, 1), LAUNCH_HEADROOM_CAP);
        assert_eq!(capacity.headroom, Some(64));
        assert_eq!(capacity.verdict, LaunchVerdict::Proceed);
        // A projection that first trips at k = 65 is still the cap.
        let edge = lanes(Some(u64::MAX / 2), Some(0), 64, 1_000);
        assert_eq!(
            launch_headroom(Some(&edge), 0, cost(0, 1), LAUNCH_HEADROOM_CAP).headroom,
            Some(64)
        );
        // Every ceiling well past the projection: still the cap.
        assert!(matches!(
            projected_verdict(Some(&roomy), 64, cost(1, 1)),
            SpawnGate::Proceed
        ));
    }

    /// The wire vocabulary coord's CHECK constraint mirrors.
    #[test]
    fn the_verdict_words_are_the_wire_vocabulary() {
        let words: Vec<&str> = [
            LaunchVerdict::Proceed,
            LaunchVerdict::Warn,
            LaunchVerdict::Critical,
            LaunchVerdict::GuardDisabled,
            LaunchVerdict::Unreadable,
        ]
        .into_iter()
        .map(LaunchVerdict::as_str)
        .collect();
        assert_eq!(
            words,
            ["proceed", "warn", "critical", "guard_disabled", "unreadable"]
        );
    }
}
