//! `restore_class`: which of r1 / r2 / r3 a session in a restart manifest falls
//! in (architecture §9.4 "Boot of build X: the restore rule", T47).
//!
//! A pure function over the manifest row and the restart's authority. The
//! authority is DERIVED here from what the record proves, never taken from the
//! manifest's label (T47: "trust the manifest's label (a reused
//! `--approved <question_id>` keeps r2 and r3)" is a listed wrong answer).
//!
//! Decisions recorded for the ambiguities the architecture leaves open:
//!
//! - **Tested in the order r3, r2, r1**, as §9.4 says.
//! - **r3 needs ALL of**: authority `operator_approved`, an executor-run
//!   restart, the full close guard True at the final re-check taken AT the stop
//!   (a missing or untaken re-check is NOT a clearance: it reads r1), an armed
//!   close arm, the session unattended with its mode positively derived, and no
//!   arrival after the re-check (which would have been promoted to r1 at boot,
//!   [`BootPromotion`]).
//! - **A mode-Unknown session is r2 only under an operator authority**; under
//!   `agent_run` EVERY row is r1 (`agent-rebuild-window`: "it restores every
//!   session it stopped").
//! - **An unreadable authority reads `agent_run`** (m4-1): the strictest
//!   reading restores the most and applies no operator-only class.

use crate::state::{Mode, Tri};

/// A restart's authority, DERIVED by [`derive_authority`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authority {
    /// The operator answered "approve" for this device and build, and the record
    /// has not been consumed by a previous boot.
    OperatorApproved,
    /// The operator prepared the restart themselves (#2403's twin); no executor
    /// runs it, so no actor takes a verdict at the kill.
    OperatorPrepared,
    /// An agent's restart, or any authority that cannot be proven.
    AgentRun,
}

/// What the restart record claims, with what proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalProof {
    /// The record's own label (NEVER trusted by itself).
    pub label: Option<Authority>,
    /// An `approve` answer exists for THIS device and THIS build.
    pub answered_approve_for_device_and_build: bool,
    /// A previous boot stamped `consumed_by_boot` on the record (NC5-4).
    pub consumed_by_previous_boot: bool,
    /// For `operator_prepared`: the prepare's `prepared_build` equals the build
    /// the boot replaced (a stale prepare, m5-1, is not honoured).
    pub prepared_build_matches: bool,
    /// The record could be read at all.
    pub readable: bool,
}

/// Derive the authority; anything unproven is `AgentRun`.
pub fn derive_authority(p: &ApprovalProof) -> Authority {
    if !p.readable {
        return Authority::AgentRun;
    }
    match p.label {
        Some(Authority::OperatorApproved)
            if p.answered_approve_for_device_and_build && !p.consumed_by_previous_boot =>
        {
            Authority::OperatorApproved
        }
        Some(Authority::OperatorPrepared) if p.prepared_build_matches => {
            Authority::OperatorPrepared
        }
        _ => Authority::AgentRun,
    }
}

/// The close guard's verdict at the stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopVerdict {
    /// The executor re-read the full guard at the stop and it read True.
    Cleared,
    /// It read, and was not True.
    NotCleared,
    /// The final re-check could not take a verdict, or no re-check ran.
    Untaken,
}

/// One manifest row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRow {
    pub session_id: String,
    pub mode: Mode,
    pub finished: bool,
    /// An explicit transcript `/effort` above the baseline (#2403's effort-held
    /// rows): an r1 row at default, but held for the operator's own sessions only.
    pub effort_above_baseline: bool,
    pub stop_verdict: StopVerdict,
    /// The restart is run by an executor (the one actor that takes the verdict).
    pub executor_run: bool,
    /// The kernel's close arm is armed for this session on the hosting build.
    pub close_arm_armed: bool,
    /// The transcript gained a turn-opening record or an unbalanced enqueue
    /// after the stamped last record, or a coord message is undelivered.
    pub arrival_after_stamp: bool,
}

/// The restore class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreClass {
    /// Restored automatically, paced, at its recorded effort.
    R1,
    /// Offered as a placeholder for the operator's Restore.
    R2 {
        mode_unknown: bool,
        effort_held: bool,
    },
    /// Not restored: a finish the guard cleared at the stop; carries a
    /// `SessionClosed` record built from the stamp.
    R3,
}

/// The boot promotion: a row that would be r3 but saw an arrival after its
/// stamp is promoted to r1 at build X's boot, before any restore (it only
/// restores MORE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootPromotion;

/// A `SessionClosed` record owed for an r3 row (audited like the Closer's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionClosedRecord {
    pub session_id: String,
    pub closed_by_restart: bool,
}

impl SessionClosedRecord {
    pub fn for_r3(row: &ManifestRow) -> SessionClosedRecord {
        SessionClosedRecord {
            session_id: row.session_id.clone(),
            closed_by_restart: true,
        }
    }
}

/// `restore_class(row, authority)`; tested r3 -> r2 -> r1.
pub fn restore_class(row: &ManifestRow, authority: Authority) -> RestoreClass {
    // Under agent_run every row is r1; r2 and r3 are operator-only (R3-E).
    if authority == Authority::AgentRun {
        return RestoreClass::R1;
    }
    // r3: executor-run operator_approved restart, a finish whose full guard read
    // True at the stop under an armed close arm, nothing arrived since.
    if authority == Authority::OperatorApproved
        && row.executor_run
        && row.finished
        && row.mode == Mode::Unattended
        && row.close_arm_armed
        && row.stop_verdict == StopVerdict::Cleared
        && !row.arrival_after_stamp
    {
        return RestoreClass::R3;
    }
    // r2: an attended session whatever its state, and a mode-Unknown one
    // (treated as attended); and an effort-held unattended row (skipped by the
    // bulk buttons, restored per row).
    match row.mode {
        Mode::Attended => RestoreClass::R2 {
            mode_unknown: false,
            effort_held: row.effort_above_baseline,
        },
        Mode::Unknown => RestoreClass::R2 {
            mode_unknown: true,
            effort_held: row.effort_above_baseline,
        },
        Mode::Unattended => RestoreClass::R1,
    }
}

/// Whether a row's restore class `R3` is demoted by the boot promotion.
pub fn boot_promotes(row: &ManifestRow, class: RestoreClass) -> Option<BootPromotion> {
    (class == RestoreClass::R3 && row.arrival_after_stamp).then_some(BootPromotion)
}

/// Whether an r1 row is effort-held (T47: the unattended `/effort max` row is
/// r1 at its recorded effort, never held; the hold is the operator's own,
/// attended sessions).
pub fn r1_is_effort_held(row: &ManifestRow) -> Tri {
    Tri::from_bool(row.mode != Mode::Unattended && row.effort_above_baseline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(mode: Mode, finished: bool, verdict: StopVerdict) -> ManifestRow {
        ManifestRow {
            session_id: "s".into(),
            mode,
            finished,
            effort_above_baseline: false,
            stop_verdict: verdict,
            executor_run: true,
            close_arm_armed: true,
            arrival_after_stamp: false,
        }
    }

    /// T47's eight rows, in its order.
    fn eight() -> Vec<ManifestRow> {
        use StopVerdict::*;
        let mut v = vec![
            row(Mode::Unattended, false, NotCleared), // unattended unfinished
            row(Mode::Unattended, true, NotCleared),  // finished, guard not True (pending message)
            row(Mode::Attended, false, NotCleared),   // attended unfinished
            row(Mode::Unattended, false, NotCleared), // `/effort max`
            row(Mode::Unattended, true, Cleared),     // full guard True at the stop
            row(Mode::Unattended, true, Untaken),     // final re-check could not take a verdict
            row(Mode::Unattended, true, Cleared),     // peer message lands after the re-check
            row(Mode::Unknown, false, NotCleared),    // mode UNKNOWN
        ];
        v[3].effort_above_baseline = true;
        v[6].arrival_after_stamp = true;
        v
    }

    #[test]
    fn t47_operator_approved_eight_rows() {
        let c: Vec<_> = eight()
            .iter()
            .map(|r| restore_class(r, Authority::OperatorApproved))
            .collect();
        assert_eq!(c[0], RestoreClass::R1);
        assert_eq!(c[1], RestoreClass::R1);
        assert!(matches!(
            c[2],
            RestoreClass::R2 {
                mode_unknown: false,
                ..
            }
        ));
        assert_eq!(
            c[3],
            RestoreClass::R1,
            "an unattended /effort max row restores at its recorded effort"
        );
        assert_eq!(c[4], RestoreClass::R3);
        assert_eq!(
            c[5],
            RestoreClass::R1,
            "a missing re-check is not a clearance"
        );
        assert_eq!(
            c[6],
            RestoreClass::R1,
            "promoted: the arrival lands after the re-check"
        );
        assert!(matches!(
            c[7],
            RestoreClass::R2 {
                mode_unknown: true,
                ..
            }
        ));
    }

    #[test]
    fn t47_agent_run_is_all_r1() {
        for r in eight() {
            assert_eq!(restore_class(&r, Authority::AgentRun), RestoreClass::R1);
        }
    }

    #[test]
    fn t47_operator_prepared_has_no_r3() {
        let mut rows = eight();
        for r in &mut rows {
            r.executor_run = false;
        }
        let c: Vec<_> = rows
            .iter()
            .map(|r| restore_class(r, Authority::OperatorPrepared))
            .collect();
        assert_eq!(c[0], RestoreClass::R1);
        assert_eq!(c[1], RestoreClass::R1);
        assert!(matches!(c[2], RestoreClass::R2 { .. }));
        assert_eq!(c[3], RestoreClass::R1);
        assert_eq!(
            c[4],
            RestoreClass::R1,
            "no actor took its verdict at the kill"
        );
        assert_eq!(c[5], RestoreClass::R1);
        assert_eq!(c[6], RestoreClass::R1);
        assert!(matches!(
            c[7],
            RestoreClass::R2 {
                mode_unknown: true,
                ..
            }
        ));
        // Even an executor-run row under `prepared` is never r3.
        let mut r = row(Mode::Unattended, true, StopVerdict::Cleared);
        r.executor_run = true;
        assert_eq!(
            restore_class(&r, Authority::OperatorPrepared),
            RestoreClass::R1
        );
    }

    #[test]
    fn r3_needs_an_armed_close_arm_and_a_positive_mode() {
        let mut r = row(Mode::Unattended, true, StopVerdict::Cleared);
        r.close_arm_armed = false;
        assert_eq!(
            restore_class(&r, Authority::OperatorApproved),
            RestoreClass::R1
        );
        let r = row(Mode::Unknown, true, StopVerdict::Cleared);
        assert!(matches!(
            restore_class(&r, Authority::OperatorApproved),
            RestoreClass::R2 { .. }
        ));
    }

    fn proof() -> ApprovalProof {
        ApprovalProof {
            label: Some(Authority::OperatorApproved),
            answered_approve_for_device_and_build: true,
            consumed_by_previous_boot: false,
            prepared_build_matches: true,
            readable: true,
        }
    }

    #[test]
    fn t47_authority_is_derived_not_trusted() {
        assert_eq!(derive_authority(&proof()), Authority::OperatorApproved);
        // A reused `--approved <question_id>` for another device or build.
        let mut p = proof();
        p.answered_approve_for_device_and_build = false;
        assert_eq!(derive_authority(&p), Authority::AgentRun);
        // A second boot of X reading a record a previous boot consumed.
        let mut p = proof();
        p.consumed_by_previous_boot = true;
        assert_eq!(derive_authority(&p), Authority::AgentRun);
        // An unreadable authority.
        let mut p = proof();
        p.readable = false;
        assert_eq!(derive_authority(&p), Authority::AgentRun);
        // A prepare, and a stale one.
        let mut p = proof();
        p.label = Some(Authority::OperatorPrepared);
        assert_eq!(derive_authority(&p), Authority::OperatorPrepared);
        p.prepared_build_matches = false;
        assert_eq!(derive_authority(&p), Authority::AgentRun);
        // No label at all.
        let mut p = proof();
        p.label = None;
        assert_eq!(derive_authority(&p), Authority::AgentRun);
    }

    #[test]
    fn r3_rows_owe_a_session_closed_record_and_a_late_arrival_promotes() {
        let r = row(Mode::Unattended, true, StopVerdict::Cleared);
        assert_eq!(
            restore_class(&r, Authority::OperatorApproved),
            RestoreClass::R3
        );
        assert!(SessionClosedRecord::for_r3(&r).closed_by_restart);
        assert!(boot_promotes(&r, RestoreClass::R3).is_none());
        let mut late = r.clone();
        late.arrival_after_stamp = true;
        assert!(boot_promotes(&late, RestoreClass::R3).is_some());
    }
}
