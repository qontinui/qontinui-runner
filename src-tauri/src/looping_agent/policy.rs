//! The supervisor's pure per-tick decision core.
//!
//! Every impure observation (is the tab alive? does the grid look idle? is a
//! context-low marker painted?) is gathered by the bin-side glue into a
//! [`TickInput`]; [`decide`] then returns exactly one [`Action`]. Keeping the
//! decision total + pure makes the cadence-gating / relaunch-policy /
//! death-respawn matrix hard-testable under `cargo test --lib`.
//!
//! ## Ordering rationale
//!
//! 1. Disabled/stopped agents are never touched (the operator switch wins).
//! 2. A dead tab respawns (behind the spawn-failure backoff) regardless of
//!    cadence — self-heal is not rate-limited by the cycle interval.
//! 3. A live tab is only ever acted on when it is IDLE and past the spawn
//!    grace: the supervisor never kills or nudges a mid-turn session.
//! 4. Context-low relaunches promptly at the next idle tick (a degraded
//!    session shouldn't run another full cycle just to honor cadence).
//! 5. The K-cycle relaunch and the nudge both wait for cadence: they START a
//!    new cycle, and cadence is the minimum spacing between cycle starts.

/// Why a fresh spawn is happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnReason {
    /// No spawn has ever been recorded for this agent (first enable, or the
    /// runtime bookkeeping was lost).
    FirstSpawn,
    /// The agent's tab/PTY died while `desired_state=running` — respawn
    /// fresh and re-read the journal (Gap-3: death-respawn).
    DeathRespawn,
}

/// Why a live tab is being torn down and respawned fresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelaunchReason {
    /// The context-low grid marker was seen (Gap-2: context-low relaunch).
    ContextLow,
    /// `relaunch_every_cycles` cycles have run since the last fresh spawn —
    /// the proactive backstop that keeps context from ever nearing the limit.
    CycleBudget,
}

/// The single action the supervisor takes this tick for one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Do nothing this tick.
    None,
    /// Spawn a fresh visible tab (no live tab exists).
    Spawn(SpawnReason),
    /// Submit the "run your next cycle" nudge into the live idle tab.
    Nudge,
    /// Close the live tab's PTY and respawn fresh (same registry row, fresh
    /// `--session-id`, prompt = "read your journal and continue").
    Relaunch(RelaunchReason),
}

/// Everything [`decide`] needs, pre-observed by the impure glue.
#[derive(Debug, Clone, Copy)]
pub struct TickInput {
    /// `def.enabled` — the master switch.
    pub enabled: bool,
    /// `def.desired_state == Running`.
    pub desired_running: bool,
    /// A live tab is currently hosting this agent (or a boot-restore of it is
    /// still pending — the glue maps "lifecycle record open but terminal not
    /// yet re-registered" to `true` so the supervisor WAITS instead of
    /// spawning a duplicate; the lifecycle poll closes a truly-dead record
    /// within a few ticks, flipping this to `false`).
    pub tab_alive: bool,
    /// Whether ANY spawn has ever been recorded (`runtime.last_spawn_at_ms`).
    pub ever_spawned: bool,
    /// Spawn-failure backoff has elapsed (always `true` when there is no
    /// failure streak). Gates ONLY the dead→spawn path so a persistent spawn
    /// failure (e.g. no authenticated account) can't hot-spin.
    pub spawn_backoff_elapsed: bool,
    /// `spawn_grace_secs` have passed since the last fresh spawn (always
    /// `true` when no spawn is recorded — e.g. an adopted tab).
    pub spawn_grace_elapsed: bool,
    /// The rendered grid looks idle / ready-for-input
    /// ([`crate::looping_agent::idle::snapshot_looks_idle`]).
    pub idle: bool,
    /// A context-low marker is painted
    /// ([`crate::looping_agent::idle::snapshot_context_low`]).
    pub context_low: bool,
    /// `cadence_secs` have passed since the last cycle START.
    pub cadence_elapsed: bool,
    /// Cycles run since the last fresh spawn (spawn counts as cycle 1).
    pub cycles_since_relaunch: u32,
    /// `lifecycle_policy.relaunch_every_cycles` (a 0 is clamped to 1).
    pub relaunch_every_cycles: u32,
}

/// The pure per-tick decision. Total over every input combination.
pub fn decide(i: &TickInput) -> Action {
    // (1) The operator switch wins: disabled/stopped agents are never touched
    // — no nudges, no respawns, existing tabs left alone.
    if !i.enabled || !i.desired_running {
        return Action::None;
    }

    // (2) Death-respawn (or first spawn): self-heal is not cadence-gated,
    // only backoff-gated.
    if !i.tab_alive {
        if !i.spawn_backoff_elapsed {
            return Action::None;
        }
        return Action::Spawn(if i.ever_spawned {
            SpawnReason::DeathRespawn
        } else {
            SpawnReason::FirstSpawn
        });
    }

    // (3) A live tab is only acted on when idle and past the spawn grace —
    // never kill or nudge a session mid-turn / mid-boot.
    if !i.spawn_grace_elapsed || !i.idle {
        return Action::None;
    }

    // (4) Context-low: relaunch promptly at the next idle tick.
    if i.context_low {
        return Action::Relaunch(RelaunchReason::ContextLow);
    }

    // (5) Cycle starts (K-cycle relaunch OR nudge) honor cadence.
    if i.cadence_elapsed {
        if i.cycles_since_relaunch >= i.relaunch_every_cycles.max(1) {
            return Action::Relaunch(RelaunchReason::CycleBudget);
        }
        return Action::Nudge;
    }

    Action::None
}

/// Escalating spawn-failure backoff: 0 failures → retry immediately;
/// otherwise `30 * 2^(failures-1)` seconds, capped at 15 minutes. Saturating
/// (never overflows).
pub fn spawn_backoff_secs(consecutive_failures: u32) -> u64 {
    const BASE: u64 = 30;
    const CAP: u64 = 900;
    if consecutive_failures == 0 {
        return 0;
    }
    let shift = (consecutive_failures - 1).min(63);
    BASE.saturating_mul(1u64.checked_shl(shift).unwrap_or(u64::MAX))
        .min(CAP)
}

/// Cadence gate: has `cadence_secs` passed since the last cycle START?
/// `None` (no cycle ever started) gates open — the first cycle should not
/// wait a full cadence.
pub fn cadence_elapsed(
    now_ms: i64,
    last_cycle_started_at_ms: Option<i64>,
    cadence_secs: u64,
) -> bool {
    match last_cycle_started_at_ms {
        None => true,
        Some(t) => now_ms.saturating_sub(t) >= (cadence_secs as i64).saturating_mul(1000),
    }
}

/// Spawn-grace gate: has `spawn_grace_secs` passed since the last fresh
/// spawn? `None` (no recorded spawn — e.g. an adopted boot-restored tab)
/// gates open.
pub fn spawn_grace_elapsed(now_ms: i64, last_spawn_at_ms: Option<i64>, grace_secs: u64) -> bool {
    match last_spawn_at_ms {
        None => true,
        Some(t) => now_ms.saturating_sub(t) >= (grace_secs as i64).saturating_mul(1000),
    }
}

/// How the glue's liveness resolution reads for one agent this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// A live terminal session is hosting the agent right now.
    Live,
    /// No live terminal, but the durable lifecycle record for the agent's
    /// pinned claude session is still OPEN — a boot-restore may be mid-
    /// flight. WAIT (treat as alive-but-not-idle) instead of spawning a
    /// duplicate; the lifecycle poll closes a truly-dead record within a few
    /// ticks, which flips this to [`Liveness::Dead`].
    PendingRestore,
    /// Confidently no tab: spawn (subject to backoff).
    Dead,
}

/// Pure liveness resolution from the glue's three observations:
///
/// - `terminal_registered`: `TerminalManager::get(runtime.terminal_id)` hit.
/// - `lifecycle_open`: the lifecycle record for `runtime.claude_session_id`
///   exists and is `state == "open"`.
/// - `lifecycle_terminal_registered`: that record's CURRENT `terminal_id`
///   (which a boot-restore rewrites) resolves to a live terminal — the
///   re-attach path after a runner restart.
pub fn resolve_liveness(
    terminal_registered: bool,
    lifecycle_open: bool,
    lifecycle_terminal_registered: bool,
) -> Liveness {
    if terminal_registered {
        return Liveness::Live;
    }
    if lifecycle_open {
        if lifecycle_terminal_registered {
            return Liveness::Live;
        }
        return Liveness::PendingRestore;
    }
    Liveness::Dead
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A baseline input representing "enabled, running, live idle tab, all
    /// gates open, mid cycle-budget" — each test perturbs one axis.
    fn base() -> TickInput {
        TickInput {
            enabled: true,
            desired_running: true,
            tab_alive: true,
            ever_spawned: true,
            spawn_backoff_elapsed: true,
            spawn_grace_elapsed: true,
            idle: true,
            context_low: false,
            cadence_elapsed: true,
            cycles_since_relaunch: 3,
            relaunch_every_cycles: 8,
        }
    }

    #[test]
    fn disabled_or_stopped_is_always_none() {
        // Even a dead tab must NOT respawn when disabled/stopped — the
        // operator switch wins over self-heal.
        for (enabled, desired_running) in [(false, true), (true, false), (false, false)] {
            let i = TickInput {
                enabled,
                desired_running,
                tab_alive: false,
                ..base()
            };
            assert_eq!(decide(&i), Action::None);
            let i2 = TickInput {
                enabled,
                desired_running,
                context_low: true,
                ..base()
            };
            assert_eq!(decide(&i2), Action::None);
        }
    }

    #[test]
    fn dead_tab_respawns_regardless_of_cadence_or_idle() {
        let i = TickInput {
            tab_alive: false,
            idle: false,
            cadence_elapsed: false,
            ..base()
        };
        assert_eq!(decide(&i), Action::Spawn(SpawnReason::DeathRespawn));
    }

    #[test]
    fn first_spawn_vs_death_respawn() {
        let first = TickInput {
            tab_alive: false,
            ever_spawned: false,
            ..base()
        };
        assert_eq!(decide(&first), Action::Spawn(SpawnReason::FirstSpawn));
        let again = TickInput {
            tab_alive: false,
            ever_spawned: true,
            ..base()
        };
        assert_eq!(decide(&again), Action::Spawn(SpawnReason::DeathRespawn));
    }

    #[test]
    fn spawn_failure_backoff_gates_respawn() {
        let i = TickInput {
            tab_alive: false,
            spawn_backoff_elapsed: false,
            ..base()
        };
        assert_eq!(decide(&i), Action::None, "hot-spin protection");
    }

    #[test]
    fn busy_tab_is_never_touched() {
        // Mid-turn: no nudge, no relaunch — even with context low and the
        // cycle budget blown.
        let i = TickInput {
            idle: false,
            context_low: true,
            cycles_since_relaunch: 99,
            ..base()
        };
        assert_eq!(decide(&i), Action::None);
    }

    #[test]
    fn spawn_grace_holds_actions_on_a_fresh_tab() {
        let i = TickInput {
            spawn_grace_elapsed: false,
            context_low: true,
            ..base()
        };
        assert_eq!(decide(&i), Action::None);
    }

    #[test]
    fn context_low_relaunches_promptly_ignoring_cadence() {
        let i = TickInput {
            context_low: true,
            cadence_elapsed: false,
            ..base()
        };
        assert_eq!(decide(&i), Action::Relaunch(RelaunchReason::ContextLow));
    }

    #[test]
    fn context_low_wins_over_cycle_budget() {
        let i = TickInput {
            context_low: true,
            cycles_since_relaunch: 99,
            ..base()
        };
        assert_eq!(decide(&i), Action::Relaunch(RelaunchReason::ContextLow));
    }

    #[test]
    fn cadence_gates_the_nudge() {
        let waiting = TickInput {
            cadence_elapsed: false,
            ..base()
        };
        assert_eq!(decide(&waiting), Action::None);
        let due = base();
        assert_eq!(decide(&due), Action::Nudge);
    }

    #[test]
    fn cycle_budget_relaunches_instead_of_nudging() {
        let at_budget = TickInput {
            cycles_since_relaunch: 8,
            ..base()
        };
        assert_eq!(
            decide(&at_budget),
            Action::Relaunch(RelaunchReason::CycleBudget)
        );
        let over = TickInput {
            cycles_since_relaunch: 9,
            ..base()
        };
        assert_eq!(decide(&over), Action::Relaunch(RelaunchReason::CycleBudget));
        let under = TickInput {
            cycles_since_relaunch: 7,
            ..base()
        };
        assert_eq!(decide(&under), Action::Nudge);
    }

    #[test]
    fn cycle_budget_relaunch_waits_for_cadence() {
        // The K-cycle relaunch STARTS a cycle, so it honors cadence too.
        let i = TickInput {
            cycles_since_relaunch: 8,
            cadence_elapsed: false,
            ..base()
        };
        assert_eq!(decide(&i), Action::None);
    }

    #[test]
    fn zero_relaunch_budget_is_clamped_to_one() {
        // A misconfigured K=0 must not relaunch-loop before the first cycle
        // even runs: cycle 1 ⇒ 1 >= max(0,1) ⇒ relaunch at the SECOND cycle
        // start, not a spawn-relaunch spin.
        let i = TickInput {
            cycles_since_relaunch: 1,
            relaunch_every_cycles: 0,
            ..base()
        };
        assert_eq!(decide(&i), Action::Relaunch(RelaunchReason::CycleBudget));
        let fresh = TickInput {
            cycles_since_relaunch: 0,
            relaunch_every_cycles: 0,
            ..base()
        };
        assert_eq!(decide(&fresh), Action::Nudge);
    }

    #[test]
    fn spawn_backoff_escalates_and_caps() {
        assert_eq!(spawn_backoff_secs(0), 0);
        assert_eq!(spawn_backoff_secs(1), 30);
        assert_eq!(spawn_backoff_secs(2), 60);
        assert_eq!(spawn_backoff_secs(3), 120);
        assert_eq!(spawn_backoff_secs(6), 900, "capped at 15 min");
        assert_eq!(spawn_backoff_secs(60), 900);
        assert_eq!(spawn_backoff_secs(u32::MAX), 900, "no overflow");
    }

    #[test]
    fn cadence_and_grace_gates() {
        // No prior cycle/spawn ⇒ gates open.
        assert!(cadence_elapsed(1_000, None, 300));
        assert!(spawn_grace_elapsed(1_000, None, 90));
        // Just started ⇒ closed.
        assert!(!cadence_elapsed(10_000, Some(9_000), 300));
        assert!(!spawn_grace_elapsed(10_000, Some(9_000), 90));
        // Exactly elapsed ⇒ open (inclusive).
        assert!(cadence_elapsed(309_000, Some(9_000), 300));
        assert!(spawn_grace_elapsed(99_000, Some(9_000), 90));
        // Clock skew (now before last) ⇒ closed, no panic.
        assert!(!cadence_elapsed(0, Some(9_000), 300));
    }

    #[test]
    fn liveness_resolution_matrix() {
        // Direct terminal hit wins.
        assert_eq!(resolve_liveness(true, false, false), Liveness::Live);
        assert_eq!(resolve_liveness(true, true, true), Liveness::Live);
        // Re-attach via the lifecycle record's rewritten terminal id.
        assert_eq!(resolve_liveness(false, true, true), Liveness::Live);
        // Open record, terminal not (yet) registered: WAIT — a boot-restore
        // may be mid-flight; spawning now would duplicate the shepherd.
        assert_eq!(
            resolve_liveness(false, true, false),
            Liveness::PendingRestore
        );
        // No terminal, record closed/absent: confidently dead.
        assert_eq!(resolve_liveness(false, false, false), Liveness::Dead);
        assert_eq!(resolve_liveness(false, false, true), Liveness::Dead);
    }
}
