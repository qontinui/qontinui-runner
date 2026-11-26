//! `POST /runner/upgrade` — the executor that runs AFTER a passing preflight
//! (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D3 steps 2, 3 and 5).
//!
//! In order, each step through [`UpgradeEffects`] so tests drive it against
//! fakes:
//!
//! 1. **Checkpoint.** Write the upgrade record and the booting marker.
//! 2. **Arm the rollback guard OUTSIDE this runner's cgroup** — a transient
//!    user timer (`systemd-run --user --on-active=<delay> --unit <guard>
//!    <from_dir>/qontinui-runner --upgrade-guard <record>`), or, with no
//!    systemd, a detached guard process in its own session — and READ BACK
//!    that it is armed. An unarmed guard aborts the upgrade: no exit ever
//!    happens without one.
//! 3. **Swap `current`** to the candidate atomically.
//! 4. **Self-managed only:** start the successor in `--upgrade-successor
//!    <record>` mode with the listener fds (`LISTEN_FDS`).
//! 5. **Handoff exit:** stage `CloseMode::Handoff` in-process through
//!    `AppState::pending_close_mode` (never this runner's own HTTP
//!    close-request door) and exit; systemd restarts the unit through the
//!    swapped link.
//!
//! A failure at any step before the exit undoes what was done (link swapped
//! back, guard cancelled) and records the upgrade `aborted`; the guard also
//! reads the record's state when it fires, so an aborted upgrade can never be
//! rolled back by a guard that failed to cancel.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::preflight::{PreflightReport, RestartPath};
use super::provenance::runner_binary_in;
use super::record::{
    self, InFlightMarker, RestartTarget, UpgradeRecord, UpgradeState, RECORD_SCHEMA,
};
use super::sys::{self, CommandRunner};

/// The guard fires this long after the exit unless the successor cancels it.
pub const DEFAULT_GUARD_DELAY: Duration = Duration::from_secs(180);

/// The prefix of the transient guard unit's name.
pub const GUARD_UNIT_PREFIX: &str = "qontinui-runner-upgrade-guard";

/// Every side effect the executor performs.
pub trait UpgradeEffects: Send + Sync {
    fn commands(&self) -> &dyn CommandRunner;
    /// Start `exe args…` detached in its own session; its pid.
    fn spawn_detached(&self, exe: &Path, args: &[String]) -> Result<u32, String>;
    /// Start the successor with `fds` at 3.. under the `LISTEN_FDS` protocol;
    /// its pid.
    fn spawn_successor(
        &self,
        exe: &Path,
        args: &[String],
        fds: &[(String, i32)],
    ) -> Result<u32, String>;
    /// Swap `link` to `target` atomically.
    fn swap_link(&self, link: &Path, target: &Path) -> Result<(), String>;
    /// Stage `CloseMode::Handoff` and begin this process's exit.
    fn stage_handoff_exit(&self) -> Result<(), String>;
}

/// What the executor needs beyond the passing preflight.
pub struct ExecuteParams<'a> {
    pub report: &'a PreflightReport,
    pub upgrades_dir: &'a Path,
    pub api_port: u16,
    pub from_pid: u32,
    /// The transient guard unit's name; default
    /// `qontinui-runner-upgrade-guard-<id>`.
    pub guard_unit: Option<String>,
    pub guard_delay: Duration,
    /// The listener fds a self-managed handoff passes.
    pub listener_fds: Vec<(String, i32)>,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// An upgrade that reached its exit.
#[derive(Debug, Clone)]
pub struct Executed {
    pub record_path: PathBuf,
    pub record: UpgradeRecord,
}

/// Why the executor stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecuteError {
    /// The record, when one was written (it now reads `aborted`).
    pub record_path: Option<PathBuf>,
    pub message: String,
}

impl ExecuteError {
    fn before_record(message: impl Into<String>) -> Self {
        Self {
            record_path: None,
            message: message.into(),
        }
    }
}

/// Is `name` a unit name systemd accepts (and nothing a shell could read)?
pub fn valid_unit_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '@'))
}

/// Run the executor. The preflight must have passed.
pub fn execute(p: ExecuteParams<'_>, fx: &dyn UpgradeEffects) -> Result<Executed, ExecuteError> {
    let report = p.report;
    if !report.ok {
        return Err(ExecuteError::before_record(format!(
            "the preflight did not pass: {}",
            report.blocking.join(", ")
        )));
    }
    let to_sha = report
        .candidate_sha
        .clone()
        .ok_or_else(|| ExecuteError::before_record("the preflight established no candidate sha"))?;
    let to_dir = std::fs::canonicalize(&report.candidate).map_err(|e| {
        ExecuteError::before_record(format!("candidate {}: {e}", report.candidate.display()))
    })?;
    let id = record::new_id(p.now);
    let guard_unit = match &report.restart_path {
        RestartPath::Systemd { .. } => {
            let name = p
                .guard_unit
                .clone()
                .unwrap_or_else(|| format!("{GUARD_UNIT_PREFIX}-{}", id.replace('.', "-")));
            if !valid_unit_name(&name) {
                return Err(ExecuteError::before_record(format!(
                    "{name:?} is not a valid unit name"
                )));
            }
            Some(name)
        }
        RestartPath::SelfManaged => None,
    };
    let from_dir = report.layout.serving_dir.clone();
    let deadline = p.now
        + chrono::Duration::from_std(p.guard_delay)
            .unwrap_or_else(|_| chrono::Duration::seconds(180));
    let mut rec = UpgradeRecord {
        schema: RECORD_SCHEMA,
        id: id.clone(),
        state: UpgradeState::InFlight,
        created_at: record::rfc3339(p.now),
        from_sha: report.serving_sha.clone(),
        to_sha,
        from_dir: from_dir.clone(),
        to_dir: to_dir.clone(),
        current_link: report.layout.current_link.clone(),
        rollback: report.rollback,
        api_port: p.api_port,
        from_pid: p.from_pid,
        restart: match &report.restart_path {
            RestartPath::Systemd { unit } => RestartTarget::SystemdUnit { unit: unit.clone() },
            RestartPath::SelfManaged => RestartTarget::SelfManaged {
                from_exe: runner_binary_in(&from_dir),
                successor_pid: None,
            },
        },
        guard_unit: guard_unit.clone(),
        guard_pid: None,
        guard_delay_secs: p.guard_delay.as_secs(),
        deadline: record::rfc3339(deadline),
        holders: report.holders.clone(),
        dies_with_runner: report.dies_with_runner.clone(),
        from_pg_schema: report.pg_schema.clone(),
        to_pg_schema: None,
        probation: None,
        outcome_reason: None,
        pg_schema_unverified: false,
        concluded_at: None,
        reported_state: None,
        reported_at: None,
        preflight: serde_json::to_value(report).unwrap_or(serde_json::Value::Null),
    };

    // 1. Checkpoint.
    let record_path = record::record_path(p.upgrades_dir, &id);
    record::write_record(&record_path, &rec).map_err(|e| {
        ExecuteError::before_record(format!("write {}: {e}", record_path.display()))
    })?;
    let mut undo = Undo::default();
    let abort = |rec: &mut UpgradeRecord, undo: &Undo, why: String| -> ExecuteError {
        undo.run(fx, rec, p.upgrades_dir);
        rec.conclude(
            UpgradeState::Aborted,
            why.clone(),
            &record::rfc3339(chrono::Utc::now()),
        );
        let _ = record::write_record(&record_path, rec);
        ExecuteError {
            record_path: Some(record_path.clone()),
            message: why,
        }
    };
    if let Err(e) = record::write_marker(p.upgrades_dir, &InFlightMarker::for_record(&rec)) {
        return Err(abort(
            &mut rec,
            &undo,
            format!("write the in-flight marker: {e}"),
        ));
    }
    undo.marker = true;

    // 2. Arm the rollback guard, and read it back.
    let from_exe = runner_binary_in(&from_dir);
    match &guard_unit {
        Some(unit) => {
            let args = vec![
                "--user".to_string(),
                format!("--on-active={}s", p.guard_delay.as_secs()),
                format!("--unit={unit}"),
                format!("--description=Qontinui runner upgrade rollback guard {id}"),
                from_exe.display().to_string(),
                "--upgrade-guard".to_string(),
                record_path.display().to_string(),
            ];
            match fx
                .commands()
                .run(Path::new(sys::SYSTEMD_RUN), &args, sys::COMMAND_TIMEOUT)
            {
                Ok(o) if o.success() => {}
                Ok(o) => {
                    return Err(abort(
                        &mut rec,
                        &undo,
                        format!("systemd-run did not arm the guard: {}", o.summary()),
                    ))
                }
                Err(e) => return Err(abort(&mut rec, &undo, format!("systemd-run: {e}"))),
            }
            undo.guard_unit = Some(unit.clone());
            let timer = format!("{unit}.timer");
            match sys::systemctl_user(fx.commands(), &["is-active", &timer]) {
                Ok(o) if o.stdout.trim() == "active" => {}
                Ok(o) => {
                    return Err(abort(
                        &mut rec,
                        &undo,
                        format!(
                            "the guard timer {timer} is not active after arming ({})",
                            o.stdout.trim()
                        ),
                    ))
                }
                Err(e) => return Err(abort(&mut rec, &undo, format!("reading back {timer}: {e}"))),
            }
        }
        None => {
            let args = vec![
                "--upgrade-guard".to_string(),
                record_path.display().to_string(),
                "--after".to_string(),
                p.guard_delay.as_secs().to_string(),
            ];
            match fx.spawn_detached(&from_exe, &args) {
                Ok(pid) => {
                    rec.guard_pid = Some(pid);
                    if let Err(e) = record::write_record(&record_path, &rec) {
                        return Err(abort(&mut rec, &undo, format!("record the guard pid: {e}")));
                    }
                }
                Err(e) => {
                    return Err(abort(
                        &mut rec,
                        &undo,
                        format!("start the guard process: {e}"),
                    ))
                }
            }
        }
    }

    // 3. Swap `current` to the candidate.
    if let Err(e) = fx.swap_link(&rec.current_link, &to_dir) {
        let msg = format!("swap {}: {e}", rec.current_link.display());
        return Err(abort(&mut rec, &undo, msg));
    }
    undo.link_back_to = Some(from_dir.clone());

    // 4. Self-managed: start the successor with the listener fds.
    if let RestartTarget::SelfManaged { from_exe, .. } = &rec.restart {
        let from_exe = from_exe.clone();
        let args = vec![
            "--upgrade-successor".to_string(),
            record_path.display().to_string(),
        ];
        match fx.spawn_successor(&runner_binary_in(&to_dir), &args, &p.listener_fds) {
            Ok(pid) => {
                rec.restart = RestartTarget::SelfManaged {
                    from_exe,
                    successor_pid: Some(pid),
                };
                if let Err(e) = record::write_record(&record_path, &rec) {
                    // The successor is already waiting for this process's
                    // exit; without the pid in the record the guard could not
                    // end it, so do not exit — the successor gives up on its
                    // own when this process outlives its wait.
                    return Err(abort(
                        &mut rec,
                        &undo,
                        format!("record the successor pid: {e}"),
                    ));
                }
            }
            Err(e) => return Err(abort(&mut rec, &undo, format!("start the successor: {e}"))),
        }
    }

    // 5. The handoff exit.
    if let Err(e) = fx.stage_handoff_exit() {
        return Err(abort(
            &mut rec,
            &undo,
            format!("stage the handoff exit: {e}"),
        ));
    }
    Ok(Executed {
        record_path,
        record: rec,
    })
}

/// What [`execute`] must reverse on an abort.
#[derive(Default)]
struct Undo {
    marker: bool,
    guard_unit: Option<String>,
    link_back_to: Option<PathBuf>,
}

impl Undo {
    fn run(&self, fx: &dyn UpgradeEffects, rec: &UpgradeRecord, dir: &Path) {
        if let Some(back) = &self.link_back_to {
            if let Err(e) = fx.swap_link(&rec.current_link, back) {
                tracing::error!(error = %e, "runner upgrade abort: could not swap current back");
            }
        }
        if let Some(unit) = &self.guard_unit {
            let _ = cancel_guard_unit(fx.commands(), unit);
        }
        if self.marker {
            let _ = record::remove_marker(dir);
        }
    }
}

/// Stop the guard's transient timer and service. Absent units are fine.
pub fn cancel_guard_unit(runner: &dyn CommandRunner, unit: &str) -> Result<(), String> {
    let timer = format!("{unit}.timer");
    let service = format!("{unit}.service");
    let out = sys::systemctl_user(runner, &["stop", &timer, &service])?;
    if out.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl --user stop {timer} {service}: {}",
            out.summary()
        ))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runner_upgrade::preflight::evaluate;
    use crate::runner_upgrade::preflight::tests::{
        fixture, healthy_census, healthy_runner, inputs, FakeRunner,
    };
    use std::sync::Mutex;

    /// Fake effects over a [`FakeRunner`]; the link swap is the REAL one
    /// (against the fixture's temp dir).
    pub(crate) struct FakeEffects {
        pub runner: FakeRunner,
        pub log: Mutex<Vec<String>>,
        pub fail_exit: bool,
    }

    impl FakeEffects {
        pub(crate) fn new(runner: FakeRunner) -> Self {
            Self {
                runner,
                log: Mutex::new(Vec::new()),
                fail_exit: false,
            }
        }
        fn log(&self, s: String) {
            self.log.lock().unwrap().push(s);
        }
        pub(crate) fn logged(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl UpgradeEffects for FakeEffects {
        fn commands(&self) -> &dyn CommandRunner {
            &self.runner
        }
        fn spawn_detached(&self, exe: &Path, args: &[String]) -> Result<u32, String> {
            self.log(format!("detached {} {}", exe.display(), args.join(" ")));
            Ok(7001)
        }
        fn spawn_successor(
            &self,
            exe: &Path,
            args: &[String],
            fds: &[(String, i32)],
        ) -> Result<u32, String> {
            self.log(format!(
                "successor {} {} fds={fds:?}",
                exe.display(),
                args.join(" ")
            ));
            Ok(7002)
        }
        fn swap_link(&self, link: &Path, target: &Path) -> Result<(), String> {
            self.log(format!("swap -> {}", target.display()));
            sys::swap_link_atomically(link, target).map_err(|e| e.to_string())
        }
        fn stage_handoff_exit(&self) -> Result<(), String> {
            self.log("handoff-exit".into());
            if self.fail_exit {
                Err("no main window".into())
            } else {
                Ok(())
            }
        }
    }

    fn armed(runner: &FakeRunner) {
        runner
            .exit("systemd-run", "--on-active=", 0)
            .ok(
                "systemctl",
                "is-active qontinui-runner-upgrade-guard",
                "active\n",
            )
            .exit("systemctl", "stop qontinui-runner-upgrade-guard", 0);
    }

    #[cfg(unix)]
    #[test]
    fn systemd_upgrade_checkpoints_arms_the_guard_swaps_and_exits_in_order() {
        let f = fixture();
        let report = evaluate(inputs(&f, Ok(healthy_census())), &healthy_runner(&f));
        assert!(report.ok);
        let runner = FakeRunner::default();
        armed(&runner);
        let fx = FakeEffects::new(runner);
        let dir = tempfile::tempdir().unwrap();
        let done = execute(
            ExecuteParams {
                report: &report,
                upgrades_dir: dir.path(),
                api_port: 9911,
                from_pid: 4242,
                guard_unit: None,
                guard_delay: Duration::from_secs(180),
                listener_fds: vec![],
                now: chrono::DateTime::parse_from_rfc3339("2026-11-24T22:00:00Z")
                    .unwrap()
                    .into(),
            },
            &fx,
        )
        .unwrap();
        // The record and the marker are on disk.
        let rec = record::read_record(&done.record_path).unwrap();
        assert_eq!(rec.state, UpgradeState::InFlight);
        assert_eq!(rec.holders.len(), 1);
        assert_eq!(rec.from_pg_schema.as_deref(), Some("md5:abc/10"));
        assert_eq!(rec.deadline, "2026-11-24T22:03:00Z");
        let marker = record::read_marker(dir.path()).unwrap();
        assert_eq!(marker.state, "in_flight");
        assert_eq!(marker.port, 9911);
        // The guard is a transient user timer running the FROM build.
        let calls = fx.runner.calls.lock().unwrap().clone();
        let arm = calls.iter().find(|c| c.starts_with("systemd-run")).unwrap();
        assert!(arm.contains("--on-active=180s"), "{arm}");
        assert!(
            arm.contains(&format!(
                "{}/qontinui-runner --upgrade-guard",
                f.layout.serving_dir.display()
            )),
            "{arm}"
        );
        assert!(arm.contains(&done.record_path.display().to_string()));
        // Then the swap, then the exit — and the link now names the candidate.
        assert_eq!(
            fx.logged(),
            vec![
                format!("swap -> {}", f.candidate.display()),
                "handoff-exit".to_string()
            ]
        );
        assert_eq!(
            sys::link_target(&f.layout.current_link).unwrap(),
            f.candidate
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_guard_that_does_not_read_back_armed_aborts_before_any_swap() {
        let f = fixture();
        let report = evaluate(inputs(&f, Ok(healthy_census())), &healthy_runner(&f));
        let runner = FakeRunner::default();
        runner
            .exit("systemd-run", "--on-active=", 0)
            .ok("systemctl", "is-active", "inactive\n")
            .exit("systemctl", "stop", 0);
        let fx = FakeEffects::new(runner);
        let dir = tempfile::tempdir().unwrap();
        let err = execute(
            ExecuteParams {
                report: &report,
                upgrades_dir: dir.path(),
                api_port: 9911,
                from_pid: 1,
                guard_unit: Some("qr-guard-scratch".into()),
                guard_delay: Duration::from_secs(30),
                listener_fds: vec![],
                now: chrono::Utc::now(),
            },
            &fx,
        )
        .unwrap_err();
        assert!(err.message.contains("not active"), "{}", err.message);
        let rec = record::read_record(err.record_path.as_ref().unwrap()).unwrap();
        assert_eq!(rec.state, UpgradeState::Aborted);
        assert!(
            record::read_marker(dir.path()).is_none(),
            "the marker is withdrawn"
        );
        assert!(fx.logged().is_empty(), "no swap and no exit");
        assert!(fx
            .runner
            .called("stop qr-guard-scratch.timer qr-guard-scratch.service"));
        assert_eq!(
            sys::link_target(&f.layout.current_link).unwrap(),
            f.layout.serving_dir
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_exit_swaps_the_link_back_and_aborts() {
        let f = fixture();
        let report = evaluate(inputs(&f, Ok(healthy_census())), &healthy_runner(&f));
        let runner = FakeRunner::default();
        armed(&runner);
        let mut fx = FakeEffects::new(runner);
        fx.fail_exit = true;
        let dir = tempfile::tempdir().unwrap();
        let err = execute(
            ExecuteParams {
                report: &report,
                upgrades_dir: dir.path(),
                api_port: 9911,
                from_pid: 1,
                guard_unit: None,
                guard_delay: Duration::from_secs(180),
                listener_fds: vec![],
                now: chrono::Utc::now(),
            },
            &fx,
        )
        .unwrap_err();
        assert!(err.message.contains("no main window"));
        assert_eq!(
            sys::link_target(&f.layout.current_link).unwrap(),
            f.layout.serving_dir
        );
        assert_eq!(
            record::read_record(err.record_path.as_ref().unwrap())
                .unwrap()
                .state,
            UpgradeState::Aborted
        );
    }

    #[cfg(unix)]
    #[test]
    fn self_managed_starts_a_detached_guard_and_the_successor_with_the_listener_fds() {
        let f = fixture();
        let mut i = inputs(&f, Ok(healthy_census()));
        i.systemd_unit = None;
        i.self_managed_fds = 1;
        let report = evaluate(i, &healthy_runner(&f));
        assert!(report.ok, "{:#?}", report.checks);
        let fx = FakeEffects::new(FakeRunner::default());
        let dir = tempfile::tempdir().unwrap();
        let done = execute(
            ExecuteParams {
                report: &report,
                upgrades_dir: dir.path(),
                api_port: 9911,
                from_pid: 4242,
                guard_unit: None,
                guard_delay: Duration::from_secs(60),
                listener_fds: vec![("api".into(), 12)],
                now: chrono::Utc::now(),
            },
            &fx,
        )
        .unwrap();
        let log = fx.logged();
        assert!(log[0].starts_with(&format!(
            "detached {}/qontinui-runner --upgrade-guard",
            f.layout.serving_dir.display()
        )));
        assert!(log[0].ends_with("--after 60"), "{}", log[0]);
        assert!(log[2].starts_with(&format!(
            "successor {}/qontinui-runner --upgrade-successor",
            f.candidate.display()
        )));
        assert!(log[2].contains(r#"fds=[("api", 12)]"#));
        assert_eq!(log[3], "handoff-exit");
        let rec = record::read_record(&done.record_path).unwrap();
        assert_eq!(rec.guard_pid, Some(7001));
        assert_eq!(
            rec.restart,
            RestartTarget::SelfManaged {
                from_exe: runner_binary_in(&f.layout.serving_dir),
                successor_pid: Some(7002)
            }
        );
        assert!(rec.guard_unit.is_none());
    }

    #[test]
    fn a_failing_preflight_is_refused_before_anything_is_written() {
        let f = fixture();
        let report = evaluate(
            inputs(&f, Err("census unreadable".into())),
            &FakeRunner::default(),
        );
        assert!(!report.ok);
        let fx = FakeEffects::new(FakeRunner::default());
        let dir = tempfile::tempdir().unwrap();
        let err = execute(
            ExecuteParams {
                report: &report,
                upgrades_dir: dir.path(),
                api_port: 9911,
                from_pid: 1,
                guard_unit: None,
                guard_delay: DEFAULT_GUARD_DELAY,
                listener_fds: vec![],
                now: chrono::Utc::now(),
            },
            &fx,
        )
        .unwrap_err();
        assert!(err.record_path.is_none());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
        assert!(fx.logged().is_empty());
    }

    #[test]
    fn unit_names_are_restricted() {
        assert!(valid_unit_name(
            "qontinui-runner-upgrade-guard-20261124T220000-000Z"
        ));
        assert!(!valid_unit_name("a b"));
        assert!(!valid_unit_name("x;rm -rf"));
        assert!(!valid_unit_name(""));
    }
}
