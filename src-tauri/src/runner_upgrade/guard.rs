//! `qontinui-runner --upgrade-guard <record> [--after <secs>]` — the rollback
//! guard (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D3 step 5).
//!
//! The OLD build arms it before its exit, outside its own cgroup, so neither
//! the exit nor the successor can take it down: a transient systemd user timer
//! (the timer IS the delay), or a detached process that sleeps `--after`
//! seconds itself. The successor cancels it once it writes
//! `probation_passed`. If it fires anyway, the successor did not prove itself
//! in time, and the guard:
//!
//! 1. reads the record — a state other than `in_flight` / `probation_failed`
//!    means there is nothing to do (passed, aborted, already rolled back);
//! 2. refuses to roll back across an embedded-PG schema change (the old build
//!    may not read what the successor wrote) — it records `rollback_refused`
//!    and leaves the escalation to the report (plan Risks);
//! 3. re-points `current` at the old build;
//! 4. restarts the runner: `systemctl --user restart <unit>` (this is the
//!    guard's job and only the guard's, against the unit the record names), or
//!    self-managed, ends the successor and starts the old binary;
//! 5. records `rolled_back` with the reason, and withdraws the booting marker.
//!
//! No GUI, no Tauri, no single-instance: it runs as a plain CLI door from
//! `main` and exits.

use std::path::Path;
use std::time::Duration;

use super::record::{self, RestartTarget, UpgradeState};
use super::sys::{self, CommandRunner};

/// Every side effect the guard performs.
pub trait GuardEffects {
    fn commands(&self) -> &dyn CommandRunner;
    fn swap_link(&self, link: &Path, target: &Path) -> Result<(), String>;
    fn pid_alive(&self, pid: u32) -> bool;
    /// End `pid` (SIGTERM, then SIGKILL after a grace).
    fn terminate(&self, pid: u32) -> Result<(), String>;
    fn spawn_detached(&self, exe: &Path, args: &[String]) -> Result<u32, String>;
}

/// What the guard did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOutcome {
    /// The record was not in a state the guard acts on.
    NothingToDo(UpgradeState),
    RolledBack,
    RollbackRefused(String),
    RollbackFailed(String),
}

impl GuardOutcome {
    /// The process exit code of `--upgrade-guard`.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NothingToDo(_) | Self::RolledBack => 0,
            Self::RollbackRefused(_) => 3,
            Self::RollbackFailed(_) => 4,
        }
    }
}

/// Fire the guard for the record at `record_path`.
pub fn run_guard(record_path: &Path, fx: &dyn GuardEffects) -> Result<GuardOutcome, String> {
    let mut rec = record::read_record(record_path)?;
    let dir = record_path.parent().unwrap_or(Path::new("."));
    let now = || record::rfc3339(chrono::Utc::now());
    if !rec.state.guard_should_act() {
        return Ok(GuardOutcome::NothingToDo(rec.state));
    }
    let why_not_passed = match (&rec.state, &rec.probation) {
        (UpgradeState::ProbationFailed, Some(p)) => {
            format!("the successor failed probation: {}", p.failures.join("; "))
        }
        (UpgradeState::ProbationFailed, None) => "the successor failed probation".to_string(),
        _ => format!(
            "the successor {} did not pass probation within {}s",
            rec.to_sha, rec.guard_delay_secs
        ),
    };

    // The rollback rule: never across a schema change.
    match super::pg_schema::changed(rec.from_pg_schema.as_deref(), rec.to_pg_schema.as_deref()) {
        Some(true) => {
            let reason = format!(
                "{why_not_passed}; rollback REFUSED: the successor changed the embedded-PG schema ({} -> {}), \
                 which the old build {} may not read — escalated for an operator",
                rec.from_pg_schema.as_deref().unwrap_or("?"),
                rec.to_pg_schema.as_deref().unwrap_or("?"),
                rec.from_sha
            );
            rec.conclude(UpgradeState::RollbackRefused, reason.clone(), &now());
            record::write_record(record_path, &rec).map_err(|e| e.to_string())?;
            let _ = record::remove_marker(dir);
            return Ok(GuardOutcome::RollbackRefused(reason));
        }
        Some(false) => {}
        None => rec.pg_schema_unverified = true,
    }

    let fail = |rec: &mut record::UpgradeRecord, msg: String| -> Result<GuardOutcome, String> {
        let reason = format!("{why_not_passed}; rollback FAILED: {msg}");
        rec.conclude(UpgradeState::RollbackFailed, reason.clone(), &now());
        record::write_record(record_path, rec).map_err(|e| e.to_string())?;
        Ok(GuardOutcome::RollbackFailed(reason))
    };

    if let Err(e) = fx.swap_link(&rec.current_link, &rec.from_dir) {
        let msg = format!("re-point {}: {e}", rec.current_link.display());
        return fail(&mut rec, msg);
    }
    match rec.restart.clone() {
        RestartTarget::SystemdUnit { unit } => {
            match sys::systemctl_user(fx.commands(), &["restart", &unit]) {
                Ok(o) if o.success() => {}
                Ok(o) => {
                    return fail(
                        &mut rec,
                        format!("systemctl --user restart {unit}: {}", o.summary()),
                    )
                }
                Err(e) => return fail(&mut rec, e),
            }
        }
        RestartTarget::SelfManaged {
            from_exe,
            successor_pid,
        } => {
            if let Some(pid) = successor_pid.filter(|p| fx.pid_alive(*p)) {
                if let Err(e) = fx.terminate(pid) {
                    return fail(&mut rec, format!("end the successor {pid}: {e}"));
                }
            }
            if let Err(e) = fx.spawn_detached(&from_exe, &[]) {
                return fail(&mut rec, format!("start {}: {e}", from_exe.display()));
            }
        }
    }
    let mut reason = format!("{why_not_passed}; rolled back to {}", rec.from_sha);
    if rec.pg_schema_unverified {
        reason.push_str(
            "; the successor recorded no embedded-PG schema, so the rollback could not be checked against a \
             schema change",
        );
    }
    rec.conclude(UpgradeState::RolledBack, reason, &now());
    record::write_record(record_path, &rec).map_err(|e| e.to_string())?;
    let _ = record::remove_marker(dir);
    Ok(GuardOutcome::RolledBack)
}

/// The real effects of a guard process.
pub struct RealGuardEffects {
    pub runner: sys::RealCommandRunner,
}

impl GuardEffects for RealGuardEffects {
    fn commands(&self) -> &dyn CommandRunner {
        &self.runner
    }
    fn swap_link(&self, link: &Path, target: &Path) -> Result<(), String> {
        sys::swap_link_atomically(link, target).map_err(|e| e.to_string())
    }
    fn pid_alive(&self, pid: u32) -> bool {
        sys::pid_alive(pid)
    }
    fn terminate(&self, pid: u32) -> Result<(), String> {
        super::spawn::terminate(pid, Duration::from_secs(30))
    }
    fn spawn_detached(&self, exe: &Path, args: &[String]) -> Result<u32, String> {
        super::spawn::spawn_detached(exe, args)
    }
}

/// `--upgrade-guard <record> [--after <secs>]`: sleep (a detached guard), fire,
/// print one line, return the exit code.
pub fn guard_main(record_path: &Path, after: Option<Duration>) -> i32 {
    if let Some(d) = after {
        std::thread::sleep(d);
    }
    let fx = RealGuardEffects {
        runner: sys::RealCommandRunner,
    };
    match run_guard(record_path, &fx) {
        Ok(outcome) => {
            println!("upgrade_guard outcome={outcome:?}");
            outcome.exit_code()
        }
        Err(e) => {
            println!("upgrade_guard error={e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_upgrade::preflight::tests::FakeRunner;
    use crate::runner_upgrade::record::{
        sample_record, write_record, ProbationReport, UpgradeRecord,
    };
    use std::sync::Mutex;

    struct FakeGuard {
        runner: FakeRunner,
        alive: Vec<u32>,
        log: Mutex<Vec<String>>,
    }

    impl FakeGuard {
        fn new(runner: FakeRunner) -> Self {
            Self {
                runner,
                alive: vec![],
                log: Mutex::new(vec![]),
            }
        }
    }

    impl GuardEffects for FakeGuard {
        fn commands(&self) -> &dyn CommandRunner {
            &self.runner
        }
        fn swap_link(&self, link: &Path, target: &Path) -> Result<(), String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("swap -> {}", target.display()));
            sys::swap_link_atomically(link, target).map_err(|e| e.to_string())
        }
        fn pid_alive(&self, pid: u32) -> bool {
            self.alive.contains(&pid)
        }
        fn terminate(&self, pid: u32) -> Result<(), String> {
            self.log.lock().unwrap().push(format!("terminate {pid}"));
            Ok(())
        }
        fn spawn_detached(&self, exe: &Path, _args: &[String]) -> Result<u32, String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("start {}", exe.display()));
            Ok(1)
        }
    }

    /// A promoted root whose `current` already points at the successor, with
    /// the in-flight record and marker the old runner left.
    fn setup(
        mutate: impl FnOnce(&mut UpgradeRecord),
    ) -> (tempfile::TempDir, std::path::PathBuf, UpgradeRecord) {
        let d = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(d.path()).unwrap();
        let mut rec = sample_record(&root);
        std::fs::create_dir(&rec.from_dir).unwrap();
        std::fs::create_dir(&rec.to_dir).unwrap();
        #[cfg(unix)]
        sys::swap_link_atomically(&rec.current_link, &rec.to_dir).unwrap();
        mutate(&mut rec);
        let path = record::record_path(&root, &rec.id);
        write_record(&path, &rec).unwrap();
        record::write_marker(&root, &record::InFlightMarker::for_record(&rec)).unwrap();
        (d, path, rec)
    }

    /// A successor that never passes probation (its probation never wrote the
    /// record, like a fake binary that boots and hangs) is rolled back: the
    /// link points at the old build again and the unit is restarted.
    #[cfg(unix)]
    #[test]
    fn the_guard_rolls_back_a_successor_that_never_passed_probation() {
        let (_d, path, rec) = setup(|r| r.to_pg_schema = Some("md5:aa/10".into()));
        let runner = FakeRunner::default();
        runner.exit("systemctl", "restart qr-scratch-9911.service", 0);
        let fx = FakeGuard::new(runner);
        assert_eq!(run_guard(&path, &fx).unwrap(), GuardOutcome::RolledBack);
        assert_eq!(sys::link_target(&rec.current_link).unwrap(), rec.from_dir);
        assert!(fx
            .runner
            .called("systemctl --user restart qr-scratch-9911.service"));
        let after = record::read_record(&path).unwrap();
        assert_eq!(after.state, UpgradeState::RolledBack);
        assert!(after
            .outcome_reason
            .unwrap()
            .contains("did not pass probation within 180s"));
        assert!(!after.pg_schema_unverified);
        assert!(record::read_marker(path.parent().unwrap()).is_none());
        // Firing again is a no-op.
        assert_eq!(
            run_guard(&path, &fx).unwrap(),
            GuardOutcome::NothingToDo(UpgradeState::RolledBack)
        );
    }

    /// The same, end to end through a FAKE successor binary: a script that
    /// stands in for the candidate runner, boots, and never writes
    /// `probation_passed`. The guard runs only after it has had its chance.
    #[cfg(unix)]
    #[test]
    fn the_guard_rolls_back_after_a_fake_successor_binary_that_never_passes() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, path, rec) = setup(|r| r.to_pg_schema = r.from_pg_schema.clone());
        let fake = rec.to_dir.join("qontinui-runner");
        std::fs::write(
            &fake,
            "#!/bin/sh\n# boots, serves nothing, never passes probation\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let status = std::process::Command::new(&fake).status().unwrap();
        assert!(status.success());
        // The successor left the record untouched: still in flight.
        assert_eq!(
            record::read_record(&path).unwrap().state,
            UpgradeState::InFlight
        );
        let runner = FakeRunner::default();
        runner.exit("systemctl", "restart", 0);
        let fx = FakeGuard::new(runner);
        assert_eq!(run_guard(&path, &fx).unwrap(), GuardOutcome::RolledBack);
        assert_eq!(
            std::fs::read_link(&rec.current_link).unwrap(),
            std::path::PathBuf::from("4d524e71b")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_probation_is_rolled_back_with_its_reasons() {
        let (_d, path, _rec) = setup(|r| {
            r.state = UpgradeState::ProbationFailed;
            r.probation = Some(ProbationReport {
                failures: vec!["holder pane pane-1 was not adopted".into()],
                ..Default::default()
            });
        });
        let runner = FakeRunner::default();
        runner.exit("systemctl", "restart", 0);
        let fx = FakeGuard::new(runner);
        assert_eq!(run_guard(&path, &fx).unwrap(), GuardOutcome::RolledBack);
        let after = record::read_record(&path).unwrap();
        assert!(after
            .outcome_reason
            .unwrap()
            .contains("pane-1 was not adopted"));
        assert!(
            after.pg_schema_unverified,
            "no successor schema was recorded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_schema_change_refuses_the_rollback_and_touches_nothing() {
        let (_d, path, rec) = setup(|r| r.to_pg_schema = Some("md5:bb/11".into()));
        let fx = FakeGuard::new(FakeRunner::default());
        let out = run_guard(&path, &fx).unwrap();
        assert!(
            matches!(out, GuardOutcome::RollbackRefused(ref r) if r.contains("schema")),
            "{out:?}"
        );
        assert_eq!(out.exit_code(), 3);
        assert!(fx.log.lock().unwrap().is_empty(), "no swap, no restart");
        assert!(fx.runner.calls.lock().unwrap().is_empty());
        assert_eq!(sys::link_target(&rec.current_link).unwrap(), rec.to_dir);
        assert_eq!(
            record::read_record(&path).unwrap().state,
            UpgradeState::RollbackRefused
        );
    }

    #[cfg(unix)]
    #[test]
    fn passed_or_aborted_upgrades_are_left_alone() {
        for state in [UpgradeState::ProbationPassed, UpgradeState::Aborted] {
            let (_d, path, rec) = setup(|r| r.state = state);
            let fx = FakeGuard::new(FakeRunner::default());
            assert_eq!(
                run_guard(&path, &fx).unwrap(),
                GuardOutcome::NothingToDo(state)
            );
            assert_eq!(sys::link_target(&rec.current_link).unwrap(), rec.to_dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_restart_is_recorded_and_escalated() {
        let (_d, path, _rec) = setup(|r| r.to_pg_schema = r.from_pg_schema.clone());
        let runner = FakeRunner::default();
        runner.exit("systemctl", "restart", 1);
        let fx = FakeGuard::new(runner);
        let out = run_guard(&path, &fx).unwrap();
        assert!(matches!(out, GuardOutcome::RollbackFailed(_)));
        assert_eq!(out.exit_code(), 4);
        let after = record::read_record(&path).unwrap();
        assert_eq!(after.state, UpgradeState::RollbackFailed);
        assert!(after.report_due());
    }

    #[cfg(unix)]
    #[test]
    fn self_managed_rollback_ends_the_successor_and_starts_the_old_binary() {
        let (_d, path, rec) = setup(|r| {
            r.to_pg_schema = r.from_pg_schema.clone();
            r.restart = RestartTarget::SelfManaged {
                from_exe: r.from_dir.join("qontinui-runner"),
                successor_pid: Some(777),
            };
        });
        let mut fx = FakeGuard::new(FakeRunner::default());
        fx.alive = vec![777];
        assert_eq!(run_guard(&path, &fx).unwrap(), GuardOutcome::RolledBack);
        let log = fx.log.lock().unwrap().clone();
        assert_eq!(log[1], "terminate 777");
        assert_eq!(
            log[2],
            format!("start {}", rec.from_dir.join("qontinui-runner").display())
        );
    }
}
