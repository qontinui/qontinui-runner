//! Did a `--resume` respawn attempt ever LAUNCH the CLI?
//!
//! Plan `2026-11-10-migration-holds-for-missing-claude-cli` D4 / Phase 5. The
//! per-session migration cap ([`super::account_migration::MIGRATION_CAP`])
//! exists to stop an exhaustion chain — every account dry, the session hopping
//! for nothing. A CLI that never started is no evidence of that, so a respawn
//! whose every attempt was a launch failure gives its cap slot back.
//!
//! After plan `2026-10-20-account-migration-respawn-dies-during-cli-reinstall-churn`
//! the respawn execs the resolved binary directly, so the identity shim (and
//! its launch-failure beacon) is not in this chain: a failed exec is a spawn
//! error, and npm's placeholder stub exits 1, not 126/127. What remains as
//! evidence is the SessionStart hook: an attempt is a launch failure when
//! - its spawn failed, or
//! - its pane died with no `last_session_start_at` at or after the pane's
//!   `created_at` (re-read once after
//!   [`super::respawn_liveness::LAUNCH_STAMP_GRACE`]), while the runner's own
//!   hook carrier args were non-empty (so the hook WAS delivered and would
//!   have stamped a CLI that started).
//!
//! With empty hook carrier args, hook delivery is unknown and the attempt
//! counts as LAUNCHED — the slot is kept; an operator launch template's own
//! `--settings` in the argv is not hook delivery. An exit code (126/127) is
//! not read: it can only agree with the rule above, never overturn it.

use super::cli_availability::CliUnavailable;
use super::respawn_liveness::RespawnOutcome;

/// One pane a respawn's `spawn_next` created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnedPane {
    pub terminal_id: String,
    /// The pane's own `created_at` (unix ms), when the terminal manager still
    /// knew it right after the spawn. `None` reads as "no CLI start can be
    /// placed after it", which keeps the slot ([`death_was_launch_failure`]).
    pub created_at_ms: Option<i64>,
    /// Whether the runner's own SessionStart hook carrier args were non-empty
    /// for this spawn (never read off the whole argv).
    pub carried_settings: bool,
}

impl SpawnedPane {
    /// A pane with no launch evidence (no `created_at`, no hook carrier): its
    /// death always counts as a launched CLI.
    #[cfg(test)]
    pub(crate) fn bare(terminal_id: impl Into<String>) -> Self {
        Self {
            terminal_id: terminal_id.into(),
            created_at_ms: None,
            carried_settings: false,
        }
    }
}

/// Why `spawn_next` produced no pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnNextError {
    /// The account migration's hold ran to its ceiling with no stably
    /// launchable `claude` ([`super::cli_availability`]).
    CliUnavailable(CliUnavailable),
    /// The migration's CLI hold was ended by its `abort` while it waited:
    /// the user closed the OLD pane or a pane this respawn spawned (or, before
    /// the first spawn, the old pane went away). Nothing was spawned.
    HoldAborted,
    /// The resolve or the spawn itself failed.
    Spawn(String),
}

/// Why the FIRST spawn of a respawn produced no pane, as
/// [`RespawnOutcome::NotSpawned`] carries it. [`SpawnNextError::HoldAborted`]
/// has no variant here: `confirm_respawn` reports a first spawn whose hold was
/// aborted as `RespawnOutcome::OldPaneClosed { attempts: 0 }`, so a
/// `NotSpawned` of it cannot be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FirstSpawnError {
    CliUnavailable(CliUnavailable),
    Spawn(String),
}

impl std::fmt::Display for FirstSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliUnavailable(c) => c.fmt(f),
            Self::Spawn(e) => f.write_str(e),
        }
    }
}

impl std::fmt::Display for SpawnNextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliUnavailable(c) => c.fmt(f),
            Self::HoldAborted => f.write_str(
                "a pane of the session was closed, or the old pane exited, while the respawn waited for the Claude CLI",
            ),
            Self::Spawn(e) => f.write_str(e),
        }
    }
}

/// D4's rule for one DEAD pane: did it die without its CLI ever starting?
/// `last_session_start_at` is the session row's SessionStart stamp (unix ms),
/// read when the pane was judged dead.
pub(crate) fn death_was_launch_failure(
    pane: &SpawnedPane,
    last_session_start_at: Option<i64>,
) -> bool {
    if !pane.carried_settings {
        // Hook delivery unknown: no stamp proves nothing — counts as launched.
        return false;
    }
    let Some(created_at) = pane.created_at_ms else {
        return false;
    };
    last_session_start_at.is_none_or(|at| at < created_at)
}

/// Does this respawn outcome give back the cap slot its caller took? Only when
/// no CLI launched: the FIRST spawn failed, or every attempt was a launch
/// failure. The resume door and the respawn receiver call this after their
/// cap was taken; the migration's own arms are
/// [`super::account_migration::MigrateError::returns_cap_slot`].
pub(crate) fn outcome_never_launched(outcome: &RespawnOutcome) -> bool {
    matches!(
        outcome,
        RespawnOutcome::NotSpawned(_)
            | RespawnOutcome::Died {
                all_launch_failures: true,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(created_at_ms: Option<i64>, carried_settings: bool) -> SpawnedPane {
        SpawnedPane {
            terminal_id: "new-1".into(),
            created_at_ms,
            carried_settings,
        }
    }

    #[test]
    fn a_death_with_the_hook_delivered_and_no_stamp_since_spawn_is_a_launch_failure() {
        assert!(death_was_launch_failure(&pane(Some(1_000), true), None));
        assert!(
            death_was_launch_failure(&pane(Some(1_000), true), Some(999)),
            "a stamp from an earlier pane"
        );
    }

    #[test]
    fn a_death_after_a_session_start_stamp_is_a_launched_cli() {
        assert!(!death_was_launch_failure(
            &pane(Some(1_000), true),
            Some(1_000)
        ));
        assert!(!death_was_launch_failure(
            &pane(Some(1_000), true),
            Some(5_000)
        ));
    }

    #[test]
    fn unknown_hook_delivery_or_spawn_time_counts_as_launched() {
        assert!(!death_was_launch_failure(&pane(Some(1_000), false), None));
        assert!(!death_was_launch_failure(&pane(None, true), None));
    }

    #[test]
    fn only_a_never_launched_outcome_returns_the_slot() {
        let died = |all_launch_failures| RespawnOutcome::Died {
            exit_code: Some(1),
            attempts: 3,
            retry_spawn_error: None,
            all_launch_failures,
        };
        assert!(outcome_never_launched(&RespawnOutcome::NotSpawned(
            FirstSpawnError::Spawn("boom".into())
        )));
        assert!(outcome_never_launched(&died(true)));
        assert!(!outcome_never_launched(&died(false)));
        for kept in [
            RespawnOutcome::Alive {
                terminal_id: "t".into(),
                attempts: 1,
            },
            RespawnOutcome::Unconfirmed {
                terminal_id: "t".into(),
            },
            RespawnOutcome::ClosedByUser { attempts: 1 },
            RespawnOutcome::OldPaneClosed { attempts: 1 },
        ] {
            assert!(!outcome_never_launched(&kept), "{kept:?}");
        }
    }
}
