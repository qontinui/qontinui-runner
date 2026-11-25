//! Successor-side probation at boot (plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D3 steps 4-6).
//!
//! A runner that boots while an upgrade record names ITS sha as `to_sha`
//! proves itself before the guard fires:
//!
//! 1. it records the embedded-PG schema it booted against (`to_pg_schema`),
//!    which is what lets the guard refuse a rollback across a schema change;
//! 2. it serves `/health` with the to-sha;
//! 3. every holder pane that was live before the exit was adopted — by pane
//!    id, and where the pane is gone, its sessions are found live by SESSION
//!    ID (an account migration may move a session; a pid proves nothing);
//! 4. every `DIES-WITH-RUNNER` session is resumed through the resume door and
//!    verified by a process read.
//!
//! All pass: it writes `probation_passed`, cancels the guard and withdraws the
//! booting marker. Any fail: it writes `probation_failed` with every reason
//! and reports it to coord at once; the guard still fires and rolls back.
//!
//! The same boot task reports every concluded upgrade the operator has not
//! yet heard about (a rolled-back runner reports its own rollback), and
//! withdraws a marker no open upgrade stands behind.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{info, warn};

use super::record::{
    self, ProbationReport, ResumeOutcome, ResumeSpec, UpgradeRecord, UpgradeState,
};
use super::sys::{self, sha_agrees};

/// What probation reads from the live successor.
#[async_trait]
pub trait ProbationEnv: Send + Sync {
    /// The `gitSha` this process serves on `/health`.
    async fn served_sha(&self) -> Result<String, String>;
    /// Wait for the holder boot sweep to settle; `false` when it did not in
    /// `timeout`.
    async fn holder_sweep_settled(&self, timeout: Duration) -> bool;
    /// A holder-backed terminal with this pane id exists here.
    fn pane_adopted(&self, pane_id: &str) -> bool;
    /// Session `id` runs here now: an open record on a live terminal, or a
    /// live `claude` holding it by process read. `Err` = could not read.
    async fn session_live(&self, id: &str) -> Result<bool, String>;
    /// Resume `id` through the resume door; the door's verdict and whether it
    /// verified the resume by process read.
    async fn resume(&self, id: &str) -> (String, bool);
}

/// The probation's waits.
#[derive(Debug, Clone, Copy)]
pub struct ProbationTiming {
    pub health_attempts: u32,
    pub health_interval: Duration,
    pub sweep_timeout: Duration,
}

impl Default for ProbationTiming {
    fn default() -> Self {
        Self {
            health_attempts: 30,
            health_interval: Duration::from_secs(2),
            sweep_timeout: Duration::from_secs(90),
        }
    }
}

/// Run probation for `rec` on this successor.
pub async fn run_probation(
    rec: &UpgradeRecord,
    env: &dyn ProbationEnv,
    timing: ProbationTiming,
) -> ProbationReport {
    let mut report = ProbationReport::default();

    // /health serves the to-sha.
    let mut last = String::new();
    for attempt in 0..timing.health_attempts.max(1) {
        match env.served_sha().await {
            Ok(sha) if sha_agrees(&sha, &rec.to_sha) => {
                report.health_sha = Some(sha);
                break;
            }
            Ok(sha) => last = format!("/health serves {sha}, not the to-sha {}", rec.to_sha),
            Err(e) => last = format!("/health not read: {e}"),
        }
        if attempt + 1 < timing.health_attempts {
            tokio::time::sleep(timing.health_interval).await;
        }
    }
    if report.health_sha.is_none() {
        report.failures.push(last);
    }

    // Every holder that was live was adopted.
    if !rec.holders.is_empty() && !env.holder_sweep_settled(timing.sweep_timeout).await {
        report.failures.push(format!(
            "the holder boot sweep did not settle within {}s",
            timing.sweep_timeout.as_secs()
        ));
    }
    for h in &rec.holders {
        if env.pane_adopted(&h.pane_id) {
            continue;
        }
        // The pane is gone here; its sessions may have moved (an account
        // migration). Join by session id.
        let mut all_live = !h.session_ids.is_empty();
        for id in &h.session_ids {
            match env.session_live(id).await {
                Ok(true) => {}
                Ok(false) => all_live = false,
                Err(e) => {
                    all_live = false;
                    report
                        .failures
                        .push(format!("session {id} of pane {} not read: {e}", h.pane_id));
                }
            }
        }
        if !all_live {
            report.holders_missing.push(h.pane_id.clone());
        }
    }
    if !report.holders_missing.is_empty() {
        report.failures.push(format!(
            "holder pane(s) not adopted and their sessions not found live: {}",
            report.holders_missing.join(", ")
        ));
    }

    // Every DIES-WITH-RUNNER session resumed and verified.
    for spec in &rec.dies_with_runner {
        let outcome = resume_one(spec, env).await;
        if !outcome.verified {
            report.failures.push(format!(
                "{} {} not resumed: {}",
                outcome.kind,
                outcome.session_id.as_deref().unwrap_or("(no session id)"),
                outcome.verdict
            ));
        }
        report.resumes.push(outcome);
    }

    report.passed = report.failures.is_empty();
    report.finished_at = Some(record::rfc3339(chrono::Utc::now()));
    report
}

async fn resume_one(spec: &ResumeSpec, env: &dyn ProbationEnv) -> ResumeOutcome {
    let Some(id) = spec.session_id.clone() else {
        return ResumeOutcome {
            session_id: None,
            kind: spec.kind.clone(),
            verdict: "no session id: nothing can resume it".into(),
            verified: false,
        };
    };
    match env.session_live(&id).await {
        Ok(true) => {
            return ResumeOutcome {
                session_id: Some(id),
                kind: spec.kind.clone(),
                verdict: "already live".into(),
                verified: true,
            }
        }
        Ok(false) => {}
        Err(e) => {
            warn!(session = %id, error = %e, "upgrade probation: liveness read failed; resuming")
        }
    }
    let (verdict, verified) = env.resume(&id).await;
    ResumeOutcome {
        session_id: Some(id),
        kind: spec.kind.clone(),
        verdict,
        verified,
    }
}

/// Conclude probation in the record: passed → cancel the guard and withdraw
/// the marker; failed → `probation_failed`, the guard rolls back.
pub fn conclude(
    path: &Path,
    rec: &mut UpgradeRecord,
    report: ProbationReport,
    runner: &dyn sys::CommandRunner,
) -> Result<(), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let now = record::rfc3339(chrono::Utc::now());
    let passed = report.passed;
    let reason = if passed {
        format!("the successor {} passed probation", rec.to_sha)
    } else {
        format!(
            "the successor {} failed probation: {}",
            rec.to_sha,
            report.failures.join("; ")
        )
    };
    rec.probation = Some(report);
    rec.conclude(
        if passed {
            UpgradeState::ProbationPassed
        } else {
            UpgradeState::ProbationFailed
        },
        reason,
        &now,
    );
    record::write_record(path, rec).map_err(|e| e.to_string())?;
    if passed {
        // A failed cancel is benign: the guard reads `probation_passed` and
        // does nothing.
        if let Some(unit) = &rec.guard_unit {
            if let Err(e) = super::executor::cancel_guard_unit(runner, unit) {
                warn!(error = %e, "upgrade probation: the guard unit was not cancelled; it will find probation_passed");
            }
        }
        if let Some(pid) = rec.guard_pid {
            if sys::pid_alive(pid) {
                let _ = super::spawn::terminate(pid, Duration::from_secs(5));
            }
        }
        record::remove_marker(dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The real environment of a booted successor.
pub struct LiveProbationEnv {
    pub app: tauri::AppHandle,
    pub port: u16,
}

#[async_trait]
impl ProbationEnv for LiveProbationEnv {
    async fn served_sha(&self) -> Result<String, String> {
        let body = super::routes::read_own_health(self.port).await?;
        find_key(&body, "gitSha")
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| "/health carries no gitSha".to_string())
    }

    async fn holder_sweep_settled(&self, timeout: Duration) -> bool {
        tokio::task::spawn_blocking(move || {
            crate::terminal::holder_boot_sweep::wait_settled(timeout)
        })
        .await
        .unwrap_or(false)
    }

    fn pane_adopted(&self, pane_id: &str) -> bool {
        use tauri::Manager;
        self.app
            .try_state::<Arc<crate::terminal::TerminalManager>>()
            .and_then(|tm| tm.get(pane_id))
            .is_some_and(|s| s.is_holder_backed())
    }

    async fn session_live(&self, id: &str) -> Result<bool, String> {
        use tauri::Manager;
        let on_terminal = match (
            self.app
                .try_state::<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>(),
            self.app
                .try_state::<Arc<crate::terminal::TerminalManager>>(),
        ) {
            (Some(store), Some(tm)) => store
                .open_records()
                .iter()
                .any(|r| r.claude_session_id == id && tm.get(&r.terminal_id).is_some()),
            _ => false,
        };
        if on_terminal {
            return Ok(true);
        }
        crate::session::resume_door::live_process_holding(id)
            .await
            .map(|pid| pid.is_some())
    }

    async fn resume(&self, id: &str) -> (String, bool) {
        match crate::session::resume_door::resume_ids_in_process(&self.app, vec![id.to_string()])
            .await
        {
            Ok(report) => match report.results.into_iter().next() {
                Some(row) => (
                    row.verdict.clone(),
                    row.outcome == "resumed" && row.evidence.is_some(),
                ),
                None => ("the resume door returned no verdict".into(), false),
            },
            Err(e) => (format!("resume door unavailable: {e}"), false),
        }
    }
}

/// `key` anywhere in `v` (the `/health` body nests its fields).
fn find_key<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m
            .get(key)
            .or_else(|| m.values().find_map(|x| find_key(x, key))),
        _ => None,
    }
}

/// The boot task: probation when this process is an upgrade's successor,
/// then the outstanding reports. Primary runners only.
pub fn spawn_at_boot(app: tauri::AppHandle, port: u16) {
    tauri::async_runtime::spawn(async move {
        let dir = sys::upgrades_dir();
        let my_sha = env!("QONTINUI_GIT_SHA");
        // Records are keyed to the runner that wrote them by its API port: a
        // secondary runner sharing this QONTINUI_HOME (and perhaps this sha)
        // must never take the primary's upgrade for its own.
        match record::latest_open_record(&dir) {
            Some((path, rec)) if rec.api_port == port => {
                if rec.state == UpgradeState::InFlight && sha_agrees(&rec.to_sha, my_sha) {
                    successor_boot(&app, port, &path, rec).await;
                }
            }
            Some(_) => {}
            None => {
                // A marker with no open upgrade behind it would hold every
                // Phase 4 client in its retry loop for nothing.
                if record::read_marker(&dir).is_some_and(|m| m.port == port) {
                    let _ = record::remove_marker(&dir);
                }
            }
        }
        super::report::report_pending(&dir, port).await;
    });
}

async fn successor_boot(app: &tauri::AppHandle, port: u16, path: &PathBuf, mut rec: UpgradeRecord) {
    info!(id = %rec.id, from = %rec.from_sha, to = %rec.to_sha, "runner upgrade: this boot is the successor — probation starts");
    // 1. The schema this build booted against, before anything else can
    //    change it further.
    for attempt in 0..10 {
        match super::pg_schema::fingerprint().await {
            Ok(fp) => {
                rec.to_pg_schema = Some(fp);
                if let Err(e) = record::write_record(path, &rec) {
                    warn!(error = %e, "runner upgrade: could not record the successor's schema");
                }
                break;
            }
            Err(e) if attempt == 9 => {
                warn!(error = %e, "runner upgrade: the successor's schema was not read")
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    }
    let env = LiveProbationEnv {
        app: app.clone(),
        port,
    };
    let report = run_probation(&rec, &env, ProbationTiming::default()).await;
    let passed = report.passed;
    // Re-read: the guard may have acted while probation ran.
    match record::read_record(path) {
        Ok(fresh) if fresh.state != UpgradeState::InFlight => {
            warn!(
                state = fresh.state.as_str(),
                "runner upgrade: the record concluded while probation ran"
            );
            return;
        }
        Ok(fresh) => {
            rec = UpgradeRecord {
                to_pg_schema: rec.to_pg_schema.clone(),
                ..fresh
            }
        }
        Err(e) => warn!(error = %e, "runner upgrade: re-reading the record failed"),
    }
    if let Err(e) = conclude(path, &mut rec, report, &sys::RealCommandRunner) {
        warn!(error = %e, "runner upgrade: probation could not be recorded");
        return;
    }
    if passed {
        info!(id = %rec.id, "runner upgrade: probation passed; guard cancelled");
        return;
    }
    warn!(id = %rec.id, reason = ?rec.outcome_reason, "runner upgrade: probation FAILED; the guard will roll back");
    super::report::report_record(path, &mut rec).await;
    // If this process outlives the guard's deadline (a refused rollback
    // leaves it running), report what the guard concluded.
    let wait = chrono::DateTime::parse_from_rfc3339(&rec.deadline)
        .map(|d| {
            (d.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .num_seconds()
                .max(0) as u64
                + 60
        })
        .unwrap_or(rec.guard_delay_secs + 60);
    tokio::time::sleep(Duration::from_secs(wait)).await;
    if let Ok(mut fresh) = record::read_record(path) {
        if fresh.report_due() {
            super::report::report_record(path, &mut fresh).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_upgrade::record::{sample_record, HolderSnapshot};
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeEnv {
        sha: String,
        adopted: HashSet<String>,
        live_sessions: HashSet<String>,
        resumes: HashMap<String, (String, bool)>,
        resumed: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ProbationEnv for FakeEnv {
        async fn served_sha(&self) -> Result<String, String> {
            Ok(self.sha.clone())
        }
        async fn holder_sweep_settled(&self, _t: Duration) -> bool {
            true
        }
        fn pane_adopted(&self, pane_id: &str) -> bool {
            self.adopted.contains(pane_id)
        }
        async fn session_live(&self, id: &str) -> Result<bool, String> {
            Ok(self.live_sessions.contains(id))
        }
        async fn resume(&self, id: &str) -> (String, bool) {
            self.resumed.lock().unwrap().push(id.to_string());
            self.resumes
                .get(id)
                .cloned()
                .unwrap_or(("skipped(not_found)".into(), false))
        }
    }

    const FAST: ProbationTiming = ProbationTiming {
        health_attempts: 2,
        health_interval: Duration::from_millis(1),
        sweep_timeout: Duration::from_millis(1),
    };

    fn rec_with_two_holders() -> UpgradeRecord {
        let d = std::env::temp_dir();
        let mut r = sample_record(&d);
        r.holders.push(HolderSnapshot {
            pane_id: "pane-2".into(),
            holder_pid: Some(200),
            child_pid: Some(201),
            negotiated_version: Some(3),
            session_ids: vec!["22222222-2222-2222-2222-222222222222".into()],
        });
        r
    }

    #[tokio::test]
    async fn every_holder_adopted_and_the_to_sha_served_passes() {
        let rec = rec_with_two_holders();
        let env = FakeEnv {
            sha: rec.to_sha.clone(),
            adopted: ["pane-1".to_string(), "pane-2".to_string()].into(),
            ..Default::default()
        };
        let r = run_probation(&rec, &env, FAST).await;
        assert!(r.passed, "{:?}", r.failures);
        assert_eq!(r.health_sha.as_deref(), Some(rec.to_sha.as_str()));
    }

    #[tokio::test]
    async fn a_successor_with_a_missing_holder_fails_probation() {
        let rec = rec_with_two_holders();
        let env = FakeEnv {
            sha: rec.to_sha.clone(),
            adopted: ["pane-1".to_string()].into(),
            ..Default::default()
        };
        let r = run_probation(&rec, &env, FAST).await;
        assert!(!r.passed);
        assert_eq!(r.holders_missing, vec!["pane-2".to_string()]);
        assert!(
            r.failures.iter().any(|f| f.contains("pane-2")),
            "{:?}",
            r.failures
        );
    }

    #[tokio::test]
    async fn a_moved_session_is_joined_by_session_id_not_by_pane_or_pid() {
        let rec = rec_with_two_holders();
        let env = FakeEnv {
            sha: rec.to_sha.clone(),
            adopted: ["pane-1".to_string()].into(),
            // pane-2's session migrated to another account's terminal.
            live_sessions: ["22222222-2222-2222-2222-222222222222".to_string()].into(),
            ..Default::default()
        };
        let r = run_probation(&rec, &env, FAST).await;
        assert!(r.passed, "{:?}", r.failures);
    }

    #[tokio::test]
    async fn the_wrong_sha_on_health_fails() {
        let rec = sample_record(&std::env::temp_dir());
        let env = FakeEnv {
            sha: rec.from_sha.clone(),
            adopted: ["pane-1".to_string()].into(),
            ..Default::default()
        };
        let r = run_probation(&rec, &env, FAST).await;
        assert!(!r.passed);
        assert!(r.failures[0].contains("not the to-sha"), "{:?}", r.failures);
    }

    #[tokio::test]
    async fn dies_with_runner_sessions_are_resumed_and_an_unverified_one_fails() {
        let mut rec = sample_record(&std::env::temp_dir());
        let spec = |id: &str| ResumeSpec {
            kind: "in_process_pane".into(),
            session_id: Some(id.into()),
            pid: Some(9),
            cwd: Some("/w".into()),
            config_dir: Some("/acct".into()),
            terminal_id: Some("t".into()),
        };
        rec.dies_with_runner = vec![
            spec("aaaaaaaa-0000-0000-0000-000000000001"),
            spec("aaaaaaaa-0000-0000-0000-000000000002"),
        ];
        let mut env = FakeEnv {
            sha: rec.to_sha.clone(),
            adopted: ["pane-1".to_string()].into(),
            ..Default::default()
        };
        env.resumes.insert(
            "aaaaaaaa-0000-0000-0000-000000000001".into(),
            ("resumed".into(), true),
        );
        env.resumes.insert(
            "aaaaaaaa-0000-0000-0000-000000000002".into(),
            ("failed(respawn-died: exit 1)".into(), false),
        );
        let r = run_probation(&rec, &env, FAST).await;
        assert!(!r.passed);
        assert_eq!(r.resumes.len(), 2);
        assert!(r.resumes[0].verified);
        assert!(!r.resumes[1].verified);
        assert!(r.failures.iter().any(|f| f.contains("respawn-died")));
        assert_eq!(env.resumed.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_passed_probation_cancels_the_guard_and_withdraws_the_marker() {
        let d = tempfile::tempdir().unwrap();
        let mut rec = sample_record(d.path());
        let path = record::record_path(d.path(), &rec.id);
        record::write_record(&path, &rec).unwrap();
        record::write_marker(d.path(), &record::InFlightMarker::for_record(&rec)).unwrap();
        let runner = crate::runner_upgrade::preflight::tests::FakeRunner::default();
        runner.exit(
            "systemctl",
            "stop qontinui-runner-upgrade-guard-test.timer",
            0,
        );
        let report = ProbationReport {
            passed: true,
            ..Default::default()
        };
        conclude(&path, &mut rec, report, &runner).unwrap();
        assert_eq!(
            record::read_record(&path).unwrap().state,
            UpgradeState::ProbationPassed
        );
        assert!(runner.called("stop qontinui-runner-upgrade-guard-test.timer qontinui-runner-upgrade-guard-test.service"));
        assert!(record::read_marker(d.path()).is_none());
    }

    #[test]
    fn a_failed_probation_keeps_the_guard_and_the_marker() {
        let d = tempfile::tempdir().unwrap();
        let mut rec = sample_record(d.path());
        let path = record::record_path(d.path(), &rec.id);
        record::write_record(&path, &rec).unwrap();
        record::write_marker(d.path(), &record::InFlightMarker::for_record(&rec)).unwrap();
        let runner = crate::runner_upgrade::preflight::tests::FakeRunner::default();
        let report = ProbationReport {
            passed: false,
            failures: vec!["pane-1 missing".into()],
            ..Default::default()
        };
        conclude(&path, &mut rec, report, &runner).unwrap();
        let after = record::read_record(&path).unwrap();
        assert_eq!(after.state, UpgradeState::ProbationFailed);
        assert!(after.report_due());
        assert!(
            runner.calls.lock().unwrap().is_empty(),
            "the guard is not touched"
        );
        assert!(record::read_marker(d.path()).is_some());
    }

    #[test]
    fn health_keys_are_found_at_any_depth() {
        let v = serde_json::json!({"success": true, "data": {"gitSha": "abc1234def"}});
        assert_eq!(
            find_key(&v, "gitSha").and_then(|x| x.as_str()),
            Some("abc1234def")
        );
        assert!(find_key(&v, "nope").is_none());
    }
}
