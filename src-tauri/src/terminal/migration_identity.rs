//! Which session does a pane PROVABLY run? — the account migration's identity
//! check (plan
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`,
//! Phase 3).
//!
//! ## Why
//!
//! The migration used to trust the lifecycle record's `claude_session_id` for
//! the hinted terminal. On 2026-10-21 that record was a binder mis-bind: pane
//! `314176b7` carried `0c4e3025` while the session actually ran as pid 46416
//! in pane `601a25ac`. The migration closed `314176b7` and resumed `0c4e3025`
//! under another account — a SECOND live process on one conversation (a fork),
//! while the real host kept writing.
//!
//! Claude Code's own `<config>/sessions/<pid>.json` is the provider's
//! pid → session index. This module re-derives the identity from it, for the
//! pane's own claude pid(s), and refuses when another VALID interactive
//! process holds the same id or when nothing can be established.
//!
//! ## Shape
//!
//! - [`decide_migration_identity`] — pure, over already-validated
//!   [`RegistryRow`]s (validity: [`registry_row_validity`]).
//! - [`resolve_migration_identity`] — the I/O wrapper: ONE process snapshot,
//!   the pane's claude pids, the registry read, then the pure fn.
//!
//! - [`migration_verdict`] — pure: what the migration does with an identity,
//!   per trigger (the automatic usage-limit hint vs the operator's command).
//!
//! ## Wiring
//!
//! Both async callers of the synchronous `migrate_session` —
//! `account_migration::handle_usage_limit_hint` (before the exhaustion probe)
//! and `commands::terminal::terminal_migrate_session_account` (before
//! anything is lifted or spawned) — go through
//! `account_migration::resolve_migration_subject`, which awaits
//! [`resolve_migration_identity`], applies [`migration_verdict`], corrects the
//! record on `Rebound`, and hands the resolved `{sid, config_dir}` (as the
//! record + source account) into `migrate_session`. The same snapshot also
//! yields the old pane's `claude` anchor that the post-close delta sync waits
//! on, so the migration takes one process-table snapshot, not two.
//!
//! [`registry_row_validity`]: crate::session::claude_session_registry::registry_row_validity

use std::path::PathBuf;

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::claude_session_registry::{RegistryRow, RowValidity};
use crate::terminal::graceful_exit::ProcIdentity;

/// The migration's verdict on what the pane is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationIdentity {
    /// The pane's one valid registry row names the record's id, and no valid
    /// interactive process outside the pane holds it. `config_dir` comes from
    /// the registry row (the account the process actually runs under).
    Confirmed { sid: String, config_dir: PathBuf },
    /// The pane's one valid registry row names a DIFFERENT id than the record
    /// (an in-pane `/clear` / `/resume`, or a binder mis-bind). Migrate THAT
    /// id; the caller corrects the record.
    Rebound { sid: String, config_dir: PathBuf },
    /// Another valid live interactive process holds the id. Refuse — migrating
    /// would fork it.
    HeldElsewhere { pid: u32 },
    /// Identity could not be established. Refuse.
    Unknown(String),
}

/// Pure decision. `pane_claude_pids` are the countable claude pids in the
/// pane's inclusive subtree; `rows` are EVERY registry row with its validity
/// against the same snapshot (stale rows included — they are ignored here,
/// which is the point of carrying the verdict).
///
/// - no claude pid in the pane ⇒ `Unknown` (covers an unreadable/empty table);
/// - any pane row whose validity is UNKNOWN ⇒ `Unknown` (it might be the
///   pane's real identity);
/// - not exactly one VALID pane row ⇒ `Unknown`;
/// - a VALID interactive row outside the pane holding the id ⇒
///   `HeldElsewhere`; an UNKNOWN-validity interactive row outside the pane
///   holding it ⇒ `Unknown` (never `HeldElsewhere`, never ignored);
/// - else `Confirmed` when the id is the record's, `Rebound` when not.
pub fn decide_migration_identity(
    record_sid: &str,
    pane_claude_pids: &[u32],
    rows: &[RegistryRow],
) -> MigrationIdentity {
    if pane_claude_pids.is_empty() {
        return MigrationIdentity::Unknown(
            "no live claude process in the pane (or the process table was unreadable)".into(),
        );
    }
    let in_pane = |pid: u32| pane_claude_pids.contains(&pid);
    let pane_rows: Vec<&RegistryRow> = rows.iter().filter(|r| in_pane(r.pid)).collect();
    if let Some((r, why)) = pane_rows.iter().find_map(|r| match r.validity {
        RowValidity::Unknown(why) => Some((r, why)),
        _ => None,
    }) {
        return MigrationIdentity::Unknown(format!(
            "pane claude pid {} has a registry row whose validity is unknown: {}",
            r.pid,
            why.as_str()
        ));
    }
    let valid_pane: Vec<&&RegistryRow> =
        pane_rows.iter().filter(|r| r.validity.is_valid()).collect();
    let row = match valid_pane.as_slice() {
        [one] => **one,
        [] => {
            return MigrationIdentity::Unknown(format!(
                "no valid registry row for the pane's claude pid(s) {pane_claude_pids:?}"
            ))
        }
        many => {
            return MigrationIdentity::Unknown(format!(
                "{} valid registry rows for the pane's claude pids {:?}",
                many.len(),
                many.iter().map(|r| r.pid).collect::<Vec<_>>()
            ))
        }
    };
    let sid = row.session_id.clone();

    // Another holder: same id, outside the pane, interactive (the class the
    // 2026-10-21 census showed real forks in). Valid ⇒ refuse; unknown ⇒ we
    // cannot rule a fork out ⇒ refuse as Unknown; stale ⇒ not evidence.
    let others = rows
        .iter()
        .filter(|r| !in_pane(r.pid) && r.session_id == sid && r.is_interactive());
    let mut unknown_holder: Option<(u32, &'static str)> = None;
    for other in others {
        match other.validity {
            RowValidity::Valid => return MigrationIdentity::HeldElsewhere { pid: other.pid },
            RowValidity::Unknown(why) => {
                unknown_holder.get_or_insert((other.pid, why.as_str()));
            }
            RowValidity::Stale(_) => {}
        }
    }
    if let Some((pid, why)) = unknown_holder {
        return MigrationIdentity::Unknown(format!(
            "pid {pid} may also hold {sid} (row validity unknown: {why})"
        ));
    }

    let config_dir = row.config_dir.clone();
    if sid == record_sid {
        MigrationIdentity::Confirmed { sid, config_dir }
    } else {
        MigrationIdentity::Rebound { sid, config_dir }
    }
}

/// Snapshot-level half of the resolver, separated so it is testable without a
/// live process table: the pane's claude pids from `snapshot`, then the pure
/// decision.
pub fn resolve_from_snapshot(
    pane_child_pid: u32,
    record_sid: &str,
    snapshot: &ProcessSnapshot,
    rows: &[RegistryRow],
) -> MigrationIdentity {
    let pane_pids = crate::process_capture::process_tree::claude_pids_in_inclusive_subtree(
        pane_child_pid,
        snapshot,
    );
    decide_migration_identity(record_sid, &pane_pids, rows)
}

/// I/O resolver: take ONE process-table snapshot, find the pane's claude pids
/// (as `resume_door::read_claude_procs` does, from the pane's `child_pid`),
/// read and validate every registry row against that same snapshot, and
/// decide. `pane_child_pid = None` (the manager could not resolve the pane's
/// child) is `Unknown`.
///
/// Also returns the pane's `claude` ANCHOR (oldest claude image, pid + start
/// time) from the same snapshot: the migration's detached delta sync waits for
/// exactly that process to leave the table, and taking it here spares a second
/// snapshot. `None` when the pane hosts no claude or the table is unreadable.
pub async fn resolve_migration_identity(
    pane_child_pid: Option<u32>,
    record_sid: &str,
) -> (MigrationIdentity, Option<ProcIdentity>) {
    let Some(root) = pane_child_pid else {
        return (
            MigrationIdentity::Unknown("the pane has no resolvable child pid".into()),
            None,
        );
    };
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snapshot.names.is_empty() {
        return (
            MigrationIdentity::Unknown("process table unreadable".into()),
            None,
        );
    }
    let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
    let rows = crate::session::claude_session_registry::read_registry_rows(&config_dirs, &snapshot);
    let writer = crate::process_capture::process_tree::claude_anchor_in_subtree(root, &snapshot)
        .map(|(pid, started_at)| ProcIdentity { pid, started_at });
    (
        resolve_from_snapshot(root, record_sid, &snapshot, &rows),
        writer,
    )
}

/// Who asked for the migration. It decides ONE arm only — `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationTrigger {
    /// The PTY usage-limit hint: nobody chose this pane, so an unproven
    /// identity never authorizes closing it (plan Phase 3, "the migration does
    /// NOT act").
    UsageLimitHint,
    /// The operator's `terminal_migrate_session_account` click. The plan keeps
    /// it as the path that "still handles" a pane the automatic path refused,
    /// and the click is its own confirmation — so an UNRESOLVED identity falls
    /// back to the record (said so in a `warn!`). A PROVEN fork
    /// (`HeldElsewhere`) is refused here too: the click names a pane, not a
    /// licence to open a second live process on one conversation.
    OperatorCommand,
}

/// What the migration does with a resolved identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationVerdict {
    /// Migrate `sid` out of `config_dir`. `rebound_from` is the record's id
    /// when the provider named a different one (the caller corrects the
    /// record before migrating).
    Proceed {
        sid: String,
        config_dir: PathBuf,
        rebound_from: Option<String>,
    },
    /// Operator command with an unresolved identity: migrate the RECORD's
    /// session out of the record's account. `why` is the unresolved reason.
    ProceedOnRecord { why: String },
    /// Do not migrate. `reason` is the typed `session-account-migration-skipped`
    /// reason: `session-live-in-another-process(pid N)` or
    /// `identity-unresolved(<why>)`.
    Refuse { reason: String },
}

/// Pure: map an identity to the migration's action for `trigger`.
pub fn migration_verdict(
    identity: MigrationIdentity,
    record_sid: &str,
    trigger: MigrationTrigger,
) -> MigrationVerdict {
    match identity {
        MigrationIdentity::Confirmed { sid, config_dir } => MigrationVerdict::Proceed {
            sid,
            config_dir,
            rebound_from: None,
        },
        MigrationIdentity::Rebound { sid, config_dir } => MigrationVerdict::Proceed {
            sid,
            config_dir,
            rebound_from: Some(record_sid.to_string()),
        },
        MigrationIdentity::HeldElsewhere { pid } => MigrationVerdict::Refuse {
            reason: format!("session-live-in-another-process(pid {pid})"),
        },
        MigrationIdentity::Unknown(why) => match trigger {
            MigrationTrigger::UsageLimitHint => MigrationVerdict::Refuse {
                reason: format!("identity-unresolved({why})"),
            },
            MigrationTrigger::OperatorCommand => MigrationVerdict::ProceedOnRecord { why },
        },
    }
}

/// The roster's own spelling of `dir` when one roster entry names the same
/// path, else `dir` as a string. The usage snapshot and the cooldown map are
/// keyed by the roster's strings (`account_known_exhausted` is an exact
/// lookup), while a registry row's dir comes from the discovery sweep, which
/// may spell an env- or scan-found home differently.
pub fn roster_spelling(dir: &std::path::Path, roster: &[String]) -> String {
    roster
        .iter()
        .find(|r| std::path::Path::new(r.trim()) == dir)
        .map(|r| r.trim().to_string())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::claude_session_registry::{StaleRow, UnknownRow};

    fn row(pid: u32, sid: &str, cfg: &str, validity: RowValidity) -> RegistryRow {
        RegistryRow {
            pid,
            session_id: sid.to_string(),
            config_dir: PathBuf::from(cfg),
            kind: Some("interactive".to_string()),
            validity,
        }
    }

    const REC: &str = "0c4e3025";
    const P1_PID: u32 = 51000; // pane 314176b7's claude
    const P2_PID: u32 = 46416; // pane 601a25ac's claude — the real host

    /// THE incident (2026-10-21): the record says pane P1 runs `0c4e3025`,
    /// but P1's pid has a row for `506a0190` and pid 46416 in pane P2 is a
    /// VALID holder of `0c4e3025`. Migrating P1's "session" would fork it.
    ///
    /// Mutation check: changing the `RowValidity::Valid => HeldElsewhere` arm
    /// to `{}` makes this return `Rebound`/`Unknown` and fails.
    #[test]
    fn incident_the_real_host_is_another_pane_held_elsewhere() {
        let rows = vec![
            row(P1_PID, "506a0190", ".claude-tiohorst", RowValidity::Valid),
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
        ];
        // P1's pid names a different id → would be Rebound to 506a0190, not
        // HeldElsewhere: the HeldElsewhere guard is about the PANE's id.
        assert_eq!(
            decide_migration_identity(REC, &[P1_PID], &rows),
            MigrationIdentity::Rebound {
                sid: "506a0190".into(),
                config_dir: PathBuf::from(".claude-tiohorst"),
            },
            "P1 provably runs 506a0190 — the migration must act on THAT, not 0c4e3025"
        );

        // P1's pid has NO row: identity unresolved, never the record's guess.
        let rows_no_p1 = vec![row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid)];
        assert!(matches!(
            decide_migration_identity(REC, &[P1_PID], &rows_no_p1),
            MigrationIdentity::Unknown(_)
        ));

        // The pane that also carries a 0c4e3025 row while 46416 holds it.
        let rows_fork = vec![
            row(P1_PID, REC, ".claude-lukas", RowValidity::Valid),
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
        ];
        assert_eq!(
            decide_migration_identity(REC, &[P1_PID], &rows_fork),
            MigrationIdentity::HeldElsewhere { pid: P2_PID }
        );
    }

    /// Mutation check: swapping the final `if sid == record_sid` arms fails.
    #[test]
    fn confirmed_when_the_pane_row_names_the_record_and_nobody_else_holds_it() {
        let rows = vec![
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
            row(7, "other", ".claude-lukas", RowValidity::Valid),
        ];
        assert_eq!(
            decide_migration_identity(REC, &[P2_PID], &rows),
            MigrationIdentity::Confirmed {
                sid: REC.into(),
                config_dir: PathBuf::from(".claude-tiohorst"),
            }
        );
    }

    /// In-pane `/clear`: the same pid now reports a new id → Rebound, with the
    /// registry's config dir.
    #[test]
    fn clear_in_pane_same_pid_new_id_is_rebound() {
        let rows = vec![row(
            P2_PID,
            "new-after-clear",
            ".claude-ben",
            RowValidity::Valid,
        )];
        assert_eq!(
            decide_migration_identity(REC, &[P2_PID], &rows),
            MigrationIdentity::Rebound {
                sid: "new-after-clear".into(),
                config_dir: PathBuf::from(".claude-ben"),
            }
        );
    }

    /// Empty snapshot: no pane pids can be found → Unknown, end to end through
    /// the snapshot-level resolver.
    ///
    /// Mutation check: deleting the `pane_claude_pids.is_empty()` guard makes
    /// the pure call fall to the "no valid registry row" Unknown, so the
    /// assertion on the message's wording fails.
    #[test]
    fn empty_snapshot_is_unknown() {
        let empty = ProcessSnapshot::default();
        let rows = vec![row(
            P2_PID,
            REC,
            "c",
            RowValidity::Unknown(UnknownRow::ProcessTableUnreadable),
        )];
        match resolve_from_snapshot(P2_PID, REC, &empty, &rows) {
            MigrationIdentity::Unknown(why) => assert!(
                why.contains("no live claude process"),
                "unexpected reason: {why}"
            ),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// The pid-38920 shape: a STALE row (reused pid, procStart mismatch) for
    /// the same id outside the pane is never a holder.
    ///
    /// Mutation check: treating `Stale(_)` like `Valid` in the holder loop
    /// turns this into `HeldElsewhere { pid: 38920 }`.
    #[test]
    fn a_reused_pid_row_is_never_held_elsewhere() {
        let rows = vec![
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
            row(
                38920,
                REC,
                ".claude-tiohorst",
                RowValidity::Stale(StaleRow::ProcStartMismatch),
            ),
            row(
                51860,
                REC,
                ".claude-lukas",
                RowValidity::Stale(StaleRow::PidNotLiveClaude),
            ),
        ];
        assert_eq!(
            decide_migration_identity(REC, &[P2_PID], &rows),
            MigrationIdentity::Confirmed {
                sid: REC.into(),
                config_dir: PathBuf::from(".claude-tiohorst"),
            }
        );
    }

    /// A no-procStart row (older CLI) is UNKNOWN for the pane — whether it is
    /// the pane's own row or a possible holder elsewhere — and never
    /// `HeldElsewhere`.
    ///
    /// Mutation check: returning `HeldElsewhere` for an `Unknown(_)` holder
    /// fails the second assertion.
    #[test]
    fn a_no_proc_start_row_is_unknown_never_held_elsewhere() {
        let unknown = RowValidity::Unknown(UnknownRow::NoProcStart);
        let own = vec![row(P2_PID, REC, "c", unknown)];
        assert!(matches!(
            decide_migration_identity(REC, &[P2_PID], &own),
            MigrationIdentity::Unknown(_)
        ));
        let elsewhere = vec![
            row(P2_PID, REC, "c", RowValidity::Valid),
            row(9, REC, "d", unknown),
        ];
        match decide_migration_identity(REC, &[P2_PID], &elsewhere) {
            MigrationIdentity::Unknown(why) => assert!(why.contains("pid 9"), "{why}"),
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Two valid rows in the pane (two claude pids, both registered) — which
    /// one is "the session" is not established → Unknown. A non-interactive
    /// holder elsewhere is not a fork class.
    #[test]
    fn many_pane_rows_unknown_and_non_interactive_holder_ignored() {
        let rows = vec![
            row(1, "a", "c", RowValidity::Valid),
            row(2, "b", "c", RowValidity::Valid),
        ];
        assert!(matches!(
            decide_migration_identity("a", &[1, 2], &rows),
            MigrationIdentity::Unknown(_)
        ));

        let mut sdk = row(3, "a", "c", RowValidity::Valid);
        sdk.kind = Some("sdk".to_string());
        let rows = vec![row(1, "a", "c", RowValidity::Valid), sdk];
        assert!(matches!(
            decide_migration_identity("a", &[1], &rows),
            MigrationIdentity::Confirmed { .. }
        ));
    }

    // ── migration_verdict: one test per arm ────────────────────────────────

    /// Mutation check: making the `Confirmed` arm carry `rebound_from:
    /// Some(..)` (or dropping the registry's `config_dir`) fails this.
    #[test]
    fn verdict_confirmed_migrates_the_registry_account() {
        let id = MigrationIdentity::Confirmed {
            sid: REC.into(),
            config_dir: PathBuf::from(".claude-tiohorst"),
        };
        for trigger in [
            MigrationTrigger::UsageLimitHint,
            MigrationTrigger::OperatorCommand,
        ] {
            assert_eq!(
                migration_verdict(id.clone(), REC, trigger),
                MigrationVerdict::Proceed {
                    sid: REC.into(),
                    config_dir: PathBuf::from(".claude-tiohorst"),
                    rebound_from: None,
                }
            );
        }
    }

    /// Rebound migrates the PROVIDER's id and names the record's id so the
    /// caller corrects the record first.
    ///
    /// Mutation check: `rebound_from: None` in the `Rebound` arm fails this.
    #[test]
    fn verdict_rebound_migrates_the_provider_id_and_names_the_record_id() {
        let id = MigrationIdentity::Rebound {
            sid: "506a0190".into(),
            config_dir: PathBuf::from(".claude-tiohorst"),
        };
        assert_eq!(
            migration_verdict(id, REC, MigrationTrigger::UsageLimitHint),
            MigrationVerdict::Proceed {
                sid: "506a0190".into(),
                config_dir: PathBuf::from(".claude-tiohorst"),
                rebound_from: Some(REC.into()),
            }
        );
    }

    /// A proven second live holder refuses on BOTH triggers, with the typed
    /// reason naming the pid.
    ///
    /// Mutation check: routing `HeldElsewhere` to `ProceedOnRecord` for the
    /// operator command fails the loop's second iteration.
    #[test]
    fn verdict_held_elsewhere_refuses_on_every_trigger() {
        for trigger in [
            MigrationTrigger::UsageLimitHint,
            MigrationTrigger::OperatorCommand,
        ] {
            assert_eq!(
                migration_verdict(
                    MigrationIdentity::HeldElsewhere { pid: P2_PID },
                    REC,
                    trigger
                ),
                MigrationVerdict::Refuse {
                    reason: "session-live-in-another-process(pid 46416)".into()
                }
            );
        }
    }

    /// Unknown refuses the automatic hint (an unproven identity never closes a
    /// pane) and falls back to the record only for the operator's click.
    ///
    /// Mutation check: swapping the two `Unknown` sub-arms fails both
    /// assertions.
    #[test]
    fn verdict_unknown_refuses_the_hint_and_defers_to_the_operator() {
        let id = MigrationIdentity::Unknown("process table unreadable".into());
        assert_eq!(
            migration_verdict(id.clone(), REC, MigrationTrigger::UsageLimitHint),
            MigrationVerdict::Refuse {
                reason: "identity-unresolved(process table unreadable)".into()
            }
        );
        assert_eq!(
            migration_verdict(id, REC, MigrationTrigger::OperatorCommand),
            MigrationVerdict::ProceedOnRecord {
                why: "process table unreadable".into()
            }
        );
    }

    /// The registry's dir is re-spelled as the roster entry naming the same
    /// path, so the exact-string usage lookup finds it; an unlisted dir is
    /// kept as-is.
    ///
    /// Mutation check: returning `dir.to_string_lossy()` unconditionally
    /// fails the first assertion (the spellings differ).
    #[test]
    fn roster_spelling_prefers_the_roster_string() {
        // `./` is dropped by `Path` component comparison but kept in the
        // string, so the two spellings differ on every platform.
        let roster = vec!["/claude/./.claude-lukas".to_string()];
        let found = PathBuf::from("/claude/.claude-lukas");
        assert_eq!(roster_spelling(&found, &roster), "/claude/./.claude-lukas");
        let other = PathBuf::from("/x/.claude-ben");
        assert_eq!(
            roster_spelling(&other, &roster),
            other.to_string_lossy().into_owned()
        );
    }
}
