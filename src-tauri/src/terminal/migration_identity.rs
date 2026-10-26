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

use std::collections::HashSet;
use std::path::PathBuf;

use crate::process_capture::process_tree::ProcessSnapshot;
use crate::session::claude_session_registry::{RegistryRow, RowValidity, UnknownRow};
use crate::terminal::graceful_exit::ProcIdentity;

/// Whether this build's `procStart` clock has been checked against REAL
/// Claude Code registry files (2026-10-23 review fix 3).
///
/// Windows: yes — the FILETIME clock was validated against every live
/// `sessions/<pid>.json` in the 2026-10-21 census. Elsewhere: the Linux clock
/// (`ps -o lstart` in UTC, read from the CLI bundle — see the
/// `claude_session_registry` module doc) is implemented but has never met a
/// real Linux registry file. So off Windows a pane whose identity cannot be
/// validated is [`MigrationIdentity::Unverifiable`]: the migration keeps its
/// PRE-identity-check behaviour (migrate the runner's record, with a `warn!`
/// naming why identity was not checked) instead of silently refusing every
/// usage-limit hint — which would turn automatic migration OFF on Linux.
pub const PROC_START_CLOCK_VERIFIED: bool = cfg!(windows);

/// The migration's verdict on what the pane is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationIdentity {
    /// The pane's one valid registry row names the record's id, and no valid
    /// interactive process outside the pane holds it. `config_dir` comes from
    /// the registry row (the account the process actually runs under).
    Confirmed { sid: String, config_dir: PathBuf },
    /// The pane's one valid registry row names a DIFFERENT id than the record
    /// (an in-pane `/clear` / `/resume`, or a binder mis-bind).
    Rebound { sid: String, config_dir: PathBuf },
    /// Another valid live interactive process holds the id. Refuse — migrating
    /// would fork it.
    HeldElsewhere { pid: u32 },
    /// A live process OUTSIDE the pane has a registry row of UNKNOWN validity
    /// that may name the id (including a row too torn to read its id). A fork
    /// cannot be ruled out. Refuse.
    MaybeHeldElsewhere { pid: u32, why: String },
    /// Identity could not be established. Refuse (the operator's command
    /// falls back to the record).
    Unknown(String),
    /// Identity could not be established on a platform whose `procStart`
    /// clock is not verified ([`PROC_START_CLOCK_VERIFIED`]): proceed on the
    /// record, as before the check existed.
    Unverifiable(String),
}

/// Pure decision. `pane_claude_pids` are the countable claude pids in the
/// pane's inclusive subtree; `rows` are EVERY registry row with its validity
/// against the same snapshot (stale rows included — they are ignored here,
/// which is the point of carrying the verdict). `clock_verified` is
/// [`PROC_START_CLOCK_VERIFIED`] in production; a parameter so the off-Windows
/// arm is testable on every platform.
///
/// - a `sessions/` dir that could not be listed ⇒ unknown;
/// - an UNKNOWN-validity row for a pid that also has a VALID row is a dead
///   predecessor's file (one process writes one file) and is disregarded;
/// - no claude pid in the pane ⇒ unknown (covers an unreadable/empty table);
/// - any pane row whose validity is UNKNOWN ⇒ unknown (it might be the pane's
///   real identity);
/// - not exactly one VALID pane row ⇒ unknown;
/// - a VALID interactive row outside the pane holding the id ⇒
///   `HeldElsewhere`; an UNKNOWN-validity interactive row outside the pane
///   that names the id, or whose id could not be read ⇒ `MaybeHeldElsewhere`;
/// - else `Confirmed` when the id is the record's, `Rebound` when not.
///
/// "unknown" is [`MigrationIdentity::Unknown`] when `clock_verified`, else
/// [`MigrationIdentity::Unverifiable`].
pub fn decide_migration_identity(
    record_sid: &str,
    pane_claude_pids: &[u32],
    rows: &[RegistryRow],
    clock_verified: bool,
) -> MigrationIdentity {
    let unknown = |why: String| {
        if clock_verified {
            MigrationIdentity::Unknown(why)
        } else {
            MigrationIdentity::Unverifiable(why)
        }
    };
    if let Some(dir) = rows
        .iter()
        .find(|r| r.validity == RowValidity::Unknown(UnknownRow::RegistryDirUnreadable))
    {
        return unknown(format!(
            "the registry could not be listed under {:?}",
            dir.config_dir
        ));
    }
    if pane_claude_pids.is_empty() {
        return unknown(
            "no live claude process in the pane (or the process table was unreadable)".into(),
        );
    }
    // One process writes one file: an UNKNOWN row for a pid that also has a
    // VALID row is a dead predecessor's, not a second identity.
    let valid_pids: HashSet<u32> = rows
        .iter()
        .filter(|r| r.validity.is_valid())
        .map(|r| r.pid)
        .collect();
    let rows: Vec<&RegistryRow> = rows
        .iter()
        .filter(|r| !(matches!(r.validity, RowValidity::Unknown(_)) && valid_pids.contains(&r.pid)))
        .collect();
    let in_pane = |pid: u32| pane_claude_pids.contains(&pid);
    let pane_rows: Vec<&RegistryRow> = rows.iter().copied().filter(|r| in_pane(r.pid)).collect();
    if let Some((r, why)) = pane_rows.iter().find_map(|r| match r.validity {
        RowValidity::Unknown(why) => Some((r, why)),
        _ => None,
    }) {
        return unknown(format!(
            "pane claude pid {} has a registry row whose validity is unknown: {}",
            r.pid,
            why.as_str()
        ));
    }
    let valid_pane: Vec<&RegistryRow> = pane_rows
        .iter()
        .copied()
        .filter(|r| r.validity.is_valid())
        .collect();
    let row = match valid_pane.as_slice() {
        [one] => *one,
        [] => {
            return unknown(format!(
                "no valid registry row for the pane's claude pid(s) {pane_claude_pids:?}"
            ))
        }
        many => {
            return unknown(format!(
                "{} valid registry rows for the pane's claude pids {:?}",
                many.len(),
                many.iter().map(|r| r.pid).collect::<Vec<_>>()
            ))
        }
    };
    let sid = row.session_id.clone();

    // Another holder: outside the pane, interactive (the class the 2026-10-21
    // census showed real forks in). Valid + same id ⇒ refuse; unknown + same
    // (or unreadable) id ⇒ a fork cannot be ruled out ⇒ refuse; stale ⇒ not
    // evidence.
    let mut maybe: Option<(u32, &'static str)> = None;
    for other in rows
        .iter()
        .filter(|r| !in_pane(r.pid) && r.is_interactive())
    {
        match other.validity {
            RowValidity::Valid if other.session_id == sid => {
                return MigrationIdentity::HeldElsewhere { pid: other.pid }
            }
            RowValidity::Unknown(why) if other.session_id == sid || other.session_id.is_empty() => {
                maybe.get_or_insert((other.pid, why.as_str()));
            }
            _ => {}
        }
    }
    if let Some((pid, why)) = maybe {
        return MigrationIdentity::MaybeHeldElsewhere {
            pid,
            why: why.to_string(),
        };
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
    clock_verified: bool,
) -> MigrationIdentity {
    let pane_pids = crate::process_capture::process_tree::claude_pids_in_inclusive_subtree(
        pane_child_pid,
        snapshot,
    );
    decide_migration_identity(record_sid, &pane_pids, rows, clock_verified)
}

/// I/O resolver: take ONE process-table snapshot, find the pane's claude pids
/// (as `resume_door::read_claude_procs` does, from the pane's `child_pid`),
/// read and validate every registry row against that same snapshot, and
/// decide. `pane_child_pid = None` (the manager could not resolve the pane's
/// child) is unknown. The registry read and the decision run on the blocking
/// pool — they are disk reads (2026-10-23 review fix 8).
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
    let writer = crate::process_capture::process_tree::claude_anchor_in_subtree(root, &snapshot)
        .map(|(pid, started_at)| ProcIdentity { pid, started_at });
    let record_sid_owned = record_sid.to_string();
    let identity = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        let config_dirs = crate::terminal::transcript::find_claude_config_dirs();
        let rows =
            crate::session::claude_session_registry::read_registry_rows(&config_dirs, &snapshot);
        resolve_from_snapshot(
            root,
            &record_sid_owned,
            &snapshot,
            &rows,
            PROC_START_CLOCK_VERIFIED,
        )
    })
    .await
    .unwrap_or_else(|e| MigrationIdentity::Unknown(format!("registry read did not complete: {e}")));
    (identity, writer)
}

/// Who asked for the migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationTrigger {
    /// The PTY usage-limit hint: nobody chose this pane, so an unproven
    /// identity never authorizes closing it (plan Phase 3, "the migration does
    /// NOT act"). It migrates whatever the pane provably runs (`Rebound`).
    UsageLimitHint,
    /// The operator's `terminal_migrate_session_account` click. It NAMES a
    /// session id, so a pane provably running a DIFFERENT id is refused
    /// rather than silently migrating something else; so is a possible second
    /// holder. An identity that merely could not be established falls back to
    /// the record (the click is the confirmation), said so in a `warn!` and in
    /// the response.
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
    /// Migrate the RECORD's session out of the record's account: the identity
    /// was not established (`why`).
    ProceedOnRecord { why: String },
    /// Do not migrate. `reason` is the typed `session-account-migration-skipped`
    /// reason (and the operator command's error).
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
        MigrationIdentity::Rebound { sid, config_dir } => match trigger {
            MigrationTrigger::UsageLimitHint => MigrationVerdict::Proceed {
                sid,
                config_dir,
                rebound_from: Some(record_sid.to_string()),
            },
            MigrationTrigger::OperatorCommand => MigrationVerdict::Refuse {
                reason: format!(
                    "identity-mismatch(the pane runs session {sid}, not the requested {record_sid})"
                ),
            },
        },
        MigrationIdentity::HeldElsewhere { pid } => MigrationVerdict::Refuse {
            reason: format!("session-live-in-another-process(pid {pid})"),
        },
        MigrationIdentity::MaybeHeldElsewhere { pid, why } => MigrationVerdict::Refuse {
            reason: format!("identity-unresolved(pid {pid} may also hold the id: {why})"),
        },
        MigrationIdentity::Unknown(why) => match trigger {
            MigrationTrigger::UsageLimitHint => MigrationVerdict::Refuse {
                reason: format!("identity-unresolved({why})"),
            },
            MigrationTrigger::OperatorCommand => MigrationVerdict::ProceedOnRecord { why },
        },
        MigrationIdentity::Unverifiable(why) => MigrationVerdict::ProceedOnRecord {
            why: format!("identity not checked on this platform: {why}"),
        },
    }
}

/// Pure: a `Rebound` migrates the provider's id only when that id has a
/// transcript under the source account. Without one, rewriting the record
/// would leave two open records on the terminal and the copy would fail
/// anyway (2026-10-23 review fix 9) — refuse instead, before anything is
/// written.
pub fn rebound_transcript_gate(sid: &str, src: &str, has_transcript: bool) -> Result<(), String> {
    if has_transcript {
        Ok(())
    } else {
        Err(format!(
            "identity-unresolved(rebound id {sid} has no transcript under {src})"
        ))
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

/// Whether two spellings of a config dir name the same account, by the
/// comparison [`roster_spelling`] itself matches with: as paths, after
/// trimming — so separators, `.` segments and a trailing slash do not count.
/// Two spellings [`roster_spelling`] maps onto one roster entry are equal
/// here too.
pub fn same_config_dir(a: &str, b: &str) -> bool {
    std::path::Path::new(a.trim()) == std::path::Path::new(b.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::claude_session_registry::StaleRow;

    /// The verified-clock (Windows) decision — every pre-existing case.
    fn decide_migration_identity(
        record_sid: &str,
        pane_claude_pids: &[u32],
        rows: &[RegistryRow],
    ) -> MigrationIdentity {
        super::decide_migration_identity(record_sid, pane_claude_pids, rows, true)
    }

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
        match resolve_from_snapshot(P2_PID, REC, &empty, &rows, true) {
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
        assert_eq!(
            decide_migration_identity(REC, &[P2_PID], &elsewhere),
            MigrationIdentity::MaybeHeldElsewhere {
                pid: 9,
                why: UnknownRow::NoProcStart.as_str().to_string()
            }
        );
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

    // ── 2026-10-23 review fixes ─────────────────────────────────────────────

    fn unreadable(pid: u32, cfg: &str) -> RegistryRow {
        RegistryRow {
            pid,
            session_id: String::new(),
            config_dir: PathBuf::from(cfg),
            kind: None,
            validity: RowValidity::Unknown(UnknownRow::UnreadableFile),
        }
    }

    /// Fix 2: a live second holder whose file was caught mid-write (its id
    /// unreadable) is a POSSIBLE holder — never silently `Confirmed`. A
    /// `sessions/` dir that could not be listed is unknown outright.
    ///
    /// Mutation check: dropping `|| other.session_id.is_empty()` from the
    /// holder arm turns the first case into `Confirmed`.
    #[test]
    fn a_torn_registry_file_of_a_live_claude_may_hold_the_id() {
        let rows = vec![
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
            unreadable(51860, ".claude-lukas"),
        ];
        assert_eq!(
            decide_migration_identity(REC, &[P2_PID], &rows),
            MigrationIdentity::MaybeHeldElsewhere {
                pid: 51860,
                why: UnknownRow::UnreadableFile.as_str().to_string()
            }
        );
        let dir_down = vec![
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
            RegistryRow {
                pid: 0,
                session_id: String::new(),
                config_dir: PathBuf::from(".claude-lukas"),
                kind: None,
                validity: RowValidity::Unknown(UnknownRow::RegistryDirUnreadable),
            },
        ];
        match decide_migration_identity(REC, &[P2_PID], &dir_down) {
            MigrationIdentity::Unknown(why) => {
                assert!(why.contains("could not be listed"), "{why}")
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    /// Fix 7: one process writes one file. A pid with a VALID row plus a dead
    /// predecessor's no-procStart file (another account) is not unknown —
    /// neither for the pane nor for a holder elsewhere.
    ///
    /// Mutation check: deleting the `valid_pids` filter makes both cases
    /// unknown.
    #[test]
    fn a_dead_predecessors_file_for_a_validated_pid_is_disregarded() {
        let no_ps = RowValidity::Unknown(UnknownRow::NoProcStart);
        let rows = vec![
            row(P2_PID, REC, ".claude-tiohorst", RowValidity::Valid),
            row(P2_PID, "older-session", ".claude-lukas", no_ps),
            row(9, "other", ".claude-ben", RowValidity::Valid),
            row(9, REC, ".claude-erik", no_ps),
        ];
        assert_eq!(
            decide_migration_identity(REC, &[P2_PID], &rows),
            MigrationIdentity::Confirmed {
                sid: REC.into(),
                config_dir: PathBuf::from(".claude-tiohorst"),
            }
        );
    }

    /// Fix 3: off a verified clock, an identity that cannot be validated is
    /// `Unverifiable`, and the automatic hint migrates the record as it did
    /// before the identity check — never a silent refusal. A PROVEN holder
    /// still refuses there.
    ///
    /// Mutation check: mapping `Unverifiable` to `Refuse` for the hint fails
    /// the verdict assertion; ignoring `clock_verified` fails the first.
    #[test]
    fn off_a_verified_clock_an_unvalidatable_identity_keeps_the_old_behaviour() {
        let clock_unknown = RowValidity::Unknown(UnknownRow::MalformedProcStart);
        let rows = vec![row(P2_PID, REC, ".claude-a", clock_unknown)];
        let id = super::decide_migration_identity(REC, &[P2_PID], &rows, false);
        assert!(matches!(id, MigrationIdentity::Unverifiable(_)), "{id:?}");
        assert!(matches!(
            super::decide_migration_identity(REC, &[P2_PID], &rows, true),
            MigrationIdentity::Unknown(_)
        ));
        match migration_verdict(id, REC, MigrationTrigger::UsageLimitHint) {
            MigrationVerdict::ProceedOnRecord { why } => {
                assert!(
                    why.starts_with("identity not checked on this platform"),
                    "{why}"
                )
            }
            other => panic!("expected ProceedOnRecord, got {other:?}"),
        }
        let fork = vec![
            row(P2_PID, REC, ".claude-a", RowValidity::Valid),
            row(7, REC, ".claude-b", RowValidity::Valid),
        ];
        assert_eq!(
            super::decide_migration_identity(REC, &[P2_PID], &fork, false),
            MigrationIdentity::HeldElsewhere { pid: 7 }
        );
    }

    /// Fix 4: the operator's command names a session id. A pane provably
    /// running another id is refused, naming both ids; the hint still
    /// migrates what the pane runs.
    ///
    /// Mutation check: routing the operator's `Rebound` to `Proceed` fails.
    #[test]
    fn verdict_operator_command_refuses_a_rebound_naming_both_ids() {
        let id = MigrationIdentity::Rebound {
            sid: "506a0190".into(),
            config_dir: PathBuf::from(".claude-tiohorst"),
        };
        assert_eq!(
            migration_verdict(id.clone(), REC, MigrationTrigger::OperatorCommand),
            MigrationVerdict::Refuse {
                reason:
                    "identity-mismatch(the pane runs session 506a0190, not the requested 0c4e3025)"
                        .into()
            }
        );
        assert!(matches!(
            migration_verdict(id, REC, MigrationTrigger::UsageLimitHint),
            MigrationVerdict::Proceed {
                rebound_from: Some(_),
                ..
            }
        ));
    }

    /// Fix 4: a possible second holder refuses on BOTH triggers.
    ///
    /// Mutation check: routing `MaybeHeldElsewhere` to `ProceedOnRecord` for
    /// the operator fails the loop's second iteration.
    #[test]
    fn verdict_a_possible_second_holder_refuses_on_every_trigger() {
        for trigger in [
            MigrationTrigger::UsageLimitHint,
            MigrationTrigger::OperatorCommand,
        ] {
            assert_eq!(
                migration_verdict(
                    MigrationIdentity::MaybeHeldElsewhere {
                        pid: 9,
                        why: "x".into()
                    },
                    REC,
                    trigger
                ),
                MigrationVerdict::Refuse {
                    reason: "identity-unresolved(pid 9 may also hold the id: x)".into()
                }
            );
        }
    }

    /// Fix 9: a rebound id with no transcript refuses before any write.
    ///
    /// Mutation check: returning `Ok(())` unconditionally fails.
    #[test]
    fn a_rebound_id_without_a_transcript_is_refused() {
        assert_eq!(rebound_transcript_gate("b", "C:/a", true), Ok(()));
        assert_eq!(
            rebound_transcript_gate("b", "C:/a", false),
            Err("identity-unresolved(rebound id b has no transcript under C:/a)".into())
        );
    }
}
