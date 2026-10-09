//! Boot sweep — relaunch the conductor runs a previous runner process left
//! reading `running`.
//!
//! The reconciler is a background task, so a runner that is killed, crashes or
//! is restarted mid-run takes it along and leaves the run's durable row reading
//! `running` with no writer. Every conductor decision is re-derived from the
//! ledger each tick, so resuming is only a matter of starting a reconciler
//! again — except for the rows the dead process had IN FLIGHT. Their workers
//! lived in that process's `SessionManager` and died with it, and
//! [`compute_tick`](super::conductor::compute_tick) reads a worker missing from
//! the manager as `Gone`: it fails the row after `gone_grace_secs` and never
//! re-dispatches it. A restart during in-flight work would therefore fail the
//! run. So before relaunching, the sweep settles each such row:
//!
//! - **artifact present** → `Completed`. The report landed; only the idle
//!   signal that would have completed it was lost with the process.
//! - **no artifact** → `Submitted` again with `task_run_id` cleared and
//!   `restart_resets += 1`, so the reconciler dispatches it afresh.
//! - **already reset [`MAX_RESTART_RESETS`] times** → `Failed`, "worker lost
//!   across 2 restarts". A row that dies with every process it runs in is not
//!   re-dispatched forever.
//!
//! A live worker (its `task_run_id` IS registered in this process's
//! `SessionManager`) is left alone: the reconciler reconciles it normally.
//!
//! ## Ownership
//!
//! A temp runner and the primary share ONE embedded PG cluster, so "every
//! `running` row" includes the other instance's live runs. The sweep relaunches
//! only rows whose `owner_instance` is this instance
//! ([`local_owner_instance`]); a foreign row is some other live process's run,
//! and relaunching it would put two reconcilers on one DAG. A row with NO owner
//! predates the column: nothing can say whose it is, so it is logged and left
//! alone — never adopted by whichever instance happens to boot first. An
//! operator can still resume one explicitly through `start_orchestration_run`,
//! which claims it.
//!
//! ## Testability
//!
//! The decisions are pure ([`ownership_verdict`], [`settle_lost_workers`]) over
//! the rows, the live-session predicate and the owner. The async half
//! ([`sweep_orphaned_runs`]) applies them against PG and launches through an
//! injected [`RunLauncher`], so a PG test can count launches without a Tauri
//! app; [`run_boot_sweep`] is the live wiring.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::conductor::{drift_verdict_recorded, is_drift_verify, OrchestrationRunConfig};
use super::ledger::{local_owner_instance, Run, Subtask, SubtaskState};
use super::loop_engine::SharedLoopStates;
use crate::database::pg::orchestration::LostWorkerSettlement;
use crate::database::pg::PgDb;

/// How many times a row may be put back to `Submitted` because its worker died
/// with the runner process. The next loss fails it.
pub const MAX_RESTART_RESETS: i32 = 2;

/// Why the sweep does NOT relaunch a `running` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepSkip {
    /// The row is not `running` (the sweep's input should never carry one; the
    /// check keeps the pure function honest on its own).
    NotRunning,
    /// A reconciler for this run is already registered in this process.
    LoopRegistered,
    /// Another runner instance owns the run.
    ForeignOwner(String),
    /// The row predates `owner_instance`; it is never adopted by a sweep.
    Unowned,
}

/// One settlement the sweep will write for a `Working` row whose worker is
/// gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSettlement {
    pub task_id: String,
    /// The dead worker's id — the write is guarded on the row still carrying
    /// it, so a row something else moved in between is never clobbered.
    pub lost_task_run_id: Option<Uuid>,
    pub settlement: LostWorkerSettlement,
}

/// Whether THIS instance may relaunch `run`: `None` = relaunch it, `Some` = the
/// reason it is skipped. Pure.
pub fn ownership_verdict(run: &Run, me: &str, loop_registered: bool) -> Option<SweepSkip> {
    if run.status != "running" {
        return Some(SweepSkip::NotRunning);
    }
    if loop_registered {
        return Some(SweepSkip::LoopRegistered);
    }
    match run.owner_instance.as_deref() {
        None => Some(SweepSkip::Unowned),
        Some(owner) if owner != me => Some(SweepSkip::ForeignOwner(owner.to_string())),
        Some(_) => None,
    }
}

/// The settlements for a run's `Working` rows whose worker is not live in this
/// process (`is_live(task_run_id)` false, or no `task_run_id` at all). Rows in
/// any other state, and `Working` rows with a live worker, produce nothing.
/// Pure.
///
/// A drift-VERIFY row with an artifact but no recorded verdict is resubmitted
/// rather than completed: its completion is where the reconciler reads the
/// Digital-Twin verdict (`to_verify_drift`), and that path runs only for a
/// worker it can observe `ReadyIdle`. Completing it here would skip the verify.
///
/// A completed ELABORATOR (`emits_subtasks`) is completed here WITHOUT its
/// children being spliced. That is safe because the reconciler's harvest
/// safety net (`unharvested_elaborators`, `compute_tick` step 6) splices the
/// children of any `Completed` elaborator that has none, and the run's exit
/// waits for it — the same path that covers an elaborator a previous process
/// completed without harvesting.
pub fn settle_lost_workers(
    subtasks: &[Subtask],
    is_live: &dyn Fn(Uuid) -> bool,
) -> Vec<RowSettlement> {
    subtasks
        .iter()
        .filter(|st| st.state == SubtaskState::Working)
        .filter(|st| !st.task_run_id.is_some_and(is_live))
        .map(|st| {
            let verify_pending = is_drift_verify(st) && !drift_verdict_recorded(st);
            let report_landed = st.artifact.is_some() && !verify_pending;
            let settlement = if report_landed {
                LostWorkerSettlement::Complete
            } else if st.restart_resets + 1 > MAX_RESTART_RESETS {
                LostWorkerSettlement::Fail
            } else {
                LostWorkerSettlement::Resubmit
            };
            RowSettlement {
                task_id: st.task_id.clone(),
                lost_task_run_id: st.task_run_id,
                settlement,
            }
        })
        .collect()
}

/// Starts a reconciler for a run the sweep decided to relaunch. Injected so the
/// sweep is testable without a Tauri app.
#[async_trait]
pub trait RunLauncher: Send + Sync {
    async fn launch(&self, run: &Run, config: OrchestrationRunConfig) -> Result<(), String>;
}

/// What one sweep did, for the boot log and the tests.
#[derive(Debug, Default)]
pub struct SweepReport {
    pub relaunched: Vec<Uuid>,
    pub skipped: Vec<(Uuid, SweepSkip)>,
    /// Rows settled (moved) before relaunch, across every relaunched run.
    pub settled: Vec<(Uuid, RowSettlement)>,
    /// Runs the sweep meant to relaunch and could not, with why.
    pub failed: Vec<(Uuid, String)>,
}

/// Sweep every `running` run: settle the lost workers of each one this
/// instance (`me`) owns and has no registered loop for (`registered`), then
/// relaunch it through `launcher` with its persisted config.
///
/// A run whose settlement write fails is NOT relaunched — its `Working` rows
/// would be failed as `Gone` by the first ticks, which is the outcome the
/// sweep exists to prevent; it stays `running` for the next boot to retry.
pub async fn sweep_orphaned_runs(
    pg: &PgDb,
    me: &str,
    registered: &HashSet<Uuid>,
    is_live: &(dyn Fn(Uuid) -> bool + Send + Sync),
    launcher: &dyn RunLauncher,
) -> Result<SweepReport, String> {
    let mut report = SweepReport::default();
    for run in pg.list_running_runs().await? {
        let run_id = run.run_id;
        if let Some(skip) = ownership_verdict(&run, me, registered.contains(&run_id)) {
            match &skip {
                SweepSkip::Unowned => warn!(
                    "boot sweep: run {run_id} reads `running` but has no owner_instance (written \
                     before ownership was recorded) — left alone, never adopted; resume it \
                     explicitly with start_orchestration_run to claim it"
                ),
                SweepSkip::ForeignOwner(owner) => info!(
                    "boot sweep: run {run_id} is owned by runner instance {owner:?}, not {me:?} \
                     — left alone"
                ),
                SweepSkip::LoopRegistered | SweepSkip::NotRunning => {}
            }
            report.skipped.push((run_id, skip));
            continue;
        }

        let subtasks = match pg.list_subtasks(run_id).await {
            Ok(s) => s,
            Err(e) => {
                error!("boot sweep: run {run_id} subtasks unreadable, not relaunched: {e}");
                report.failed.push((run_id, e));
                continue;
            }
        };
        let mut settle_failed = None;
        for s in settle_lost_workers(&subtasks, is_live) {
            match pg
                .settle_lost_worker(run_id, &s.task_id, s.lost_task_run_id, s.settlement)
                .await
            {
                Ok(true) => {
                    match s.settlement {
                        LostWorkerSettlement::Complete => info!(
                            "boot sweep: run {run_id} subtask {} → completed (its report \
                             landed before the previous process died)",
                            s.task_id
                        ),
                        LostWorkerSettlement::Resubmit => info!(
                            "boot sweep: run {run_id} subtask {} → submitted (worker {:?} died \
                             with the previous process, no report)",
                            s.task_id, s.lost_task_run_id
                        ),
                        LostWorkerSettlement::Fail => warn!(
                            "boot sweep: run {run_id} subtask {} → failed: worker lost across \
                             {MAX_RESTART_RESETS} restarts",
                            s.task_id
                        ),
                    }
                    report.settled.push((run_id, s));
                }
                Ok(false) => info!(
                    "boot sweep: run {run_id} subtask {} moved before it could be settled — \
                     leaving it as it now reads",
                    s.task_id
                ),
                Err(e) => {
                    settle_failed = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = settle_failed {
            error!("boot sweep: run {run_id} lost-worker settlement failed, not relaunched: {e}");
            report.failed.push((run_id, e));
            continue;
        }

        let config = run.config.clone().unwrap_or_else(|| {
            warn!(
                "boot sweep: run {run_id} has no persisted config (written before it was \
                 recorded) — relaunching at default knobs"
            );
            OrchestrationRunConfig::default()
        });
        match launcher.launch(&run, config).await {
            Ok(()) => {
                info!("boot sweep: run {run_id} relaunched");
                report.relaunched.push(run_id);
            }
            Err(e) => {
                error!("boot sweep: run {run_id} relaunch failed: {e}");
                report.failed.push((run_id, e));
            }
        }
    }
    Ok(report)
}

/// The live launcher: the same `start_orchestration_run` path an operator's
/// start takes, so a relaunched run is indistinguishable from a re-entered one.
struct LoopEngineLauncher {
    states: SharedLoopStates,
    app_handle: tauri::AppHandle,
    pg: Arc<PgDb>,
}

#[async_trait]
impl RunLauncher for LoopEngineLauncher {
    async fn launch(&self, run: &Run, config: OrchestrationRunConfig) -> Result<(), String> {
        super::loop_engine::start_orchestration_run(
            self.states.clone(),
            self.app_handle.clone(),
            self.pg.clone(),
            run.run_id,
            &run.goal,
            run.recipe.as_deref(),
            &run.phases,
            config,
        )
        .await
        .map(|_| ())
    }
}

/// Boot wiring: run the sweep once, after PG and the `SessionManager` are up.
/// Never fails the boot — every error is logged.
pub async fn run_boot_sweep(states: SharedLoopStates, app_handle: tauri::AppHandle, pg: Arc<PgDb>) {
    let session_mgr = {
        use tauri::Manager;
        app_handle
            .try_state::<Arc<crate::claude_session::manager::SessionManager>>()
            .map(|s| s.inner().clone())
    };
    let Some(session_mgr) = session_mgr else {
        // Without it no worker's liveness can be read, and
        // `start_orchestration_run` refuses to launch anyway.
        error!("boot sweep: SessionManager state not available — no run relaunched");
        return;
    };
    let registered: HashSet<Uuid> = {
        let mgr = states.lock().await;
        mgr.loops
            .keys()
            .filter_map(|id| Uuid::parse_str(id).ok())
            .collect()
    };
    let is_live = move |trid: Uuid| session_mgr.get_state(&trid.to_string()).is_some();
    let me = local_owner_instance();
    let launcher = LoopEngineLauncher {
        states,
        app_handle,
        pg: pg.clone(),
    };
    match sweep_orphaned_runs(&pg, &me, &registered, &is_live, &launcher).await {
        Ok(r) => info!(
            "boot sweep (owner {me:?}): relaunched {}, settled {} lost worker(s), skipped {}, \
             failed {}",
            r.relaunched.len(),
            r.settled.len(),
            r.skipped.len(),
            r.failed.len()
        ),
        Err(e) => error!("boot sweep: could not list running runs: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::pg::completion_reports::CompletionReport;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn mk_run(owner: Option<&str>, status: &str) -> Run {
        Run {
            run_id: Uuid::new_v4(),
            goal: "g".to_string(),
            recipe: None,
            phases: vec!["implement".to_string()],
            status: status.to_string(),
            status_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            config: None,
            owner_instance: owner.map(str::to_string),
        }
    }

    fn mk_subtask(run_id: Uuid, task_id: &str, state: SubtaskState) -> Subtask {
        Subtask {
            task_id: task_id.to_string(),
            run_id,
            idx: 0,
            title: format!("t-{task_id}"),
            brief: "b".to_string(),
            phase: "implement".to_string(),
            repo: None,
            depends_on: vec![],
            expected_output: "a green PR".to_string(),
            emits_subtasks: false,
            state,
            task_run_id: None,
            artifact: None,
            produced_by: None,
            gate_id: None,
            gate_status: None,
            restart_resets: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn report() -> CompletionReport {
        CompletionReport {
            summary_md: "done".to_string(),
            deliverables: vec![],
            breaking_changes: vec![],
            follow_ups: vec![],
            artifacts: HashMap::new(),
        }
    }

    fn working(run_id: Uuid, task_id: &str, artifact: bool, resets: i32) -> Subtask {
        let mut st = mk_subtask(run_id, task_id, SubtaskState::Working);
        st.task_run_id = Some(Uuid::new_v4());
        st.artifact = artifact.then(report);
        st.restart_resets = resets;
        st
    }

    const NOTHING_LIVE: &dyn Fn(Uuid) -> bool = &|_| false;

    #[test]
    fn owned_running_run_without_a_loop_is_relaunched() {
        let run = mk_run(Some("primary"), "running");
        assert_eq!(ownership_verdict(&run, "primary", false), None);
    }

    #[test]
    fn foreign_unowned_registered_and_terminal_runs_are_skipped() {
        let foreign = mk_run(Some("test-9877"), "running");
        assert_eq!(
            ownership_verdict(&foreign, "primary", false),
            Some(SweepSkip::ForeignOwner("test-9877".to_string()))
        );
        let unowned = mk_run(None, "running");
        assert_eq!(
            ownership_verdict(&unowned, "primary", false),
            Some(SweepSkip::Unowned),
            "a row written before owner_instance existed is never adopted"
        );
        let mine = mk_run(Some("primary"), "running");
        assert_eq!(
            ownership_verdict(&mine, "primary", true),
            Some(SweepSkip::LoopRegistered)
        );
        let done = mk_run(Some("primary"), "complete");
        assert_eq!(
            ownership_verdict(&done, "primary", false),
            Some(SweepSkip::NotRunning)
        );
    }

    #[test]
    fn completed_and_submitted_rows_are_never_touched() {
        let run_id = Uuid::new_v4();
        let mut done = mk_subtask(run_id, "done", SubtaskState::Completed);
        done.artifact = Some(report());
        let rows = vec![
            done,
            mk_subtask(run_id, "next", SubtaskState::Submitted),
            mk_subtask(run_id, "bad", SubtaskState::Failed),
        ];
        assert!(settle_lost_workers(&rows, NOTHING_LIVE).is_empty());
    }

    #[test]
    fn lost_worker_with_a_report_completes() {
        let run_id = Uuid::new_v4();
        let st = working(run_id, "a", true, 0);
        let s = settle_lost_workers(std::slice::from_ref(&st), NOTHING_LIVE);
        assert_eq!(
            s,
            vec![RowSettlement {
                task_id: "a".to_string(),
                lost_task_run_id: st.task_run_id,
                settlement: LostWorkerSettlement::Complete,
            }]
        );
    }

    #[test]
    fn lost_worker_without_a_report_is_resubmitted_until_the_bound() {
        let run_id = Uuid::new_v4();
        let rows = vec![
            working(run_id, "first", false, 0),
            working(run_id, "second", false, 1),
            working(run_id, "third", false, MAX_RESTART_RESETS),
        ];
        let got: Vec<_> = settle_lost_workers(&rows, NOTHING_LIVE)
            .into_iter()
            .map(|s| (s.task_id, s.settlement))
            .collect();
        // "third" has already been reset MAX_RESTART_RESETS times, so one
        // more reset would exceed the bound: it fails instead.
        assert_eq!(
            got,
            vec![
                ("first".to_string(), LostWorkerSettlement::Resubmit),
                ("second".to_string(), LostWorkerSettlement::Resubmit),
                ("third".to_string(), LostWorkerSettlement::Fail),
            ]
        );
    }

    #[test]
    fn a_live_worker_is_left_to_the_reconciler() {
        let run_id = Uuid::new_v4();
        let alive = working(run_id, "alive", false, 0);
        let dead = working(run_id, "dead", false, 0);
        let alive_id = alive.task_run_id.unwrap();
        let is_live = move |id: Uuid| id == alive_id;
        let got = settle_lost_workers(&[alive, dead], &is_live);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].task_id, "dead");
    }

    #[test]
    fn a_working_row_with_no_task_run_id_is_lost() {
        let run_id = Uuid::new_v4();
        let st = mk_subtask(run_id, "unbound", SubtaskState::Working);
        let got = settle_lost_workers(&[st], &|_| true);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].settlement, LostWorkerSettlement::Resubmit);
        assert_eq!(got[0].lost_task_run_id, None);
    }

    #[test]
    fn a_resumed_run_takes_its_stored_config_and_the_callers_fanout_cache() {
        let stored = OrchestrationRunConfig {
            concurrency_cap: 8,
            report_timeout_secs: 30,
            ..OrchestrationRunConfig::default()
        };
        let caller = OrchestrationRunConfig {
            concurrency_cap: 1,
            fanout_bound: Some(5),
            ..OrchestrationRunConfig::default()
        };
        let got = caller.clone().resumed_from(Some(&stored));
        assert_eq!(got.concurrency_cap, 8, "the persisted knob wins");
        assert_eq!(got.report_timeout_secs, 30);
        assert_eq!(
            got.fanout_bound,
            Some(5),
            "the runtime cache is the caller's"
        );
        // A row written before the column existed: the caller's config.
        assert_eq!(caller.clone().resumed_from(None), caller);
    }

    #[test]
    fn an_unverified_drift_verify_report_is_rerun_not_completed() {
        let run_id = Uuid::new_v4();
        let mut st = working(run_id, "verify", true, 0);
        st.expected_output = "DriftVerdict no-drift for the runner subspace".to_string();
        // Pinned so the test cannot pass vacuously if the classifier's
        // grammar moves: it only means something for a row it recognises.
        assert!(
            is_drift_verify(&st),
            "fixture must classify as drift-verify"
        );
        let got = settle_lost_workers(&[st], NOTHING_LIVE);
        assert_eq!(got[0].settlement, LostWorkerSettlement::Resubmit);
    }

    // -- PG-backed sweep tests ------------------------------------------------
    //
    // `#[ignore]` to match the `database/pg/*` convention — they need a live PG
    // fixture (DATABASE_URL). Each one owns its rows under a UNIQUE owner name,
    // so rows another test (or a real runner sharing the cluster) left
    // `running` are foreign to it and the sweep neither settles nor launches
    // them. Run with:
    // `cargo test boot_sweep::tests -- --ignored --nocapture`

    #[derive(Default)]
    struct RecordingLauncher(Mutex<Vec<(Uuid, OrchestrationRunConfig)>>);

    #[async_trait]
    impl RunLauncher for RecordingLauncher {
        async fn launch(&self, run: &Run, config: OrchestrationRunConfig) -> Result<(), String> {
            self.0.lock().unwrap().push((run.run_id, config));
            Ok(())
        }
    }

    fn unique_owner() -> String {
        format!("sweep-test-{}", Uuid::new_v4())
    }

    async fn cleanup(pg: &PgDb, run_ids: &[Uuid]) {
        let conn = pg.pool().get().await.expect("conn");
        for id in run_ids {
            let _ = conn
                .execute("DELETE FROM orchestration.runs WHERE run_id = $1", &[id])
                .await;
        }
    }

    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn sweep_relaunches_an_owned_run_once_and_reruns_nothing() {
        let pg = PgDb::new_for_test().await;
        let me = unique_owner();
        let run_id = Uuid::new_v4();
        let cfg = OrchestrationRunConfig {
            concurrency_cap: 7,
            ..OrchestrationRunConfig::default()
        };
        pg.create_run(
            run_id,
            "resume me",
            None,
            &["implement".to_string()],
            &cfg,
            &me,
        )
        .await
        .expect("create_run");
        let mut done = mk_subtask(run_id, "done", SubtaskState::Completed);
        done.artifact = Some(report());
        done.task_run_id = Some(Uuid::new_v4());
        let next = mk_subtask(run_id, "next", SubtaskState::Submitted);
        pg.upsert_subtask(&done).await.expect("upsert done");
        pg.upsert_subtask(&next).await.expect("upsert next");

        let launcher = RecordingLauncher::default();
        let report = sweep_orphaned_runs(&pg, &me, &HashSet::new(), &|_| false, &launcher)
            .await
            .expect("sweep");

        let launched = launcher.0.lock().unwrap().clone();
        assert_eq!(launched.len(), 1, "exactly one reconciler launched");
        assert_eq!(launched[0].0, run_id);
        assert_eq!(
            launched[0].1.concurrency_cap, 7,
            "relaunched with the PERSISTED config, not the defaults"
        );
        assert!(report.settled.is_empty(), "nothing to settle");
        let rows = pg.list_subtasks(run_id).await.expect("list");
        let d = rows.iter().find(|s| s.task_id == "done").unwrap();
        assert_eq!(
            d.state,
            SubtaskState::Completed,
            "a completed row is not re-run"
        );
        assert_eq!(d.restart_resets, 0);
        let n = rows.iter().find(|s| s.task_id == "next").unwrap();
        assert_eq!(n.state, SubtaskState::Submitted);
        assert_eq!(n.task_run_id, None);

        // A registered loop means the run is already being driven here.
        let launcher2 = RecordingLauncher::default();
        let registered: HashSet<Uuid> = [run_id].into_iter().collect();
        sweep_orphaned_runs(&pg, &me, &registered, &|_| false, &launcher2)
            .await
            .expect("sweep 2");
        assert!(launcher2.0.lock().unwrap().is_empty());

        cleanup(&pg, &[run_id]).await;
    }

    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn sweep_resubmits_a_lost_worker_and_completes_a_reported_one() {
        let pg = PgDb::new_for_test().await;
        let me = unique_owner();
        let run_id = Uuid::new_v4();
        pg.create_run(
            run_id,
            "lost workers",
            None,
            &["implement".to_string()],
            &OrchestrationRunConfig::default(),
            &me,
        )
        .await
        .expect("create_run");
        let lost = working(run_id, "lost", false, 0);
        let reported = working(run_id, "reported", true, 0);
        let exhausted = working(run_id, "exhausted", false, 0);
        for st in [&lost, &reported, &exhausted] {
            pg.upsert_subtask(st).await.expect("upsert");
        }
        // restart_resets is sweep-owned (upsert never writes it) — seed the
        // exhausted row's count directly.
        pg.pool()
            .get()
            .await
            .expect("conn")
            .execute(
                "UPDATE orchestration.subtasks SET restart_resets = $3 \
                 WHERE run_id = $1 AND task_id = $2",
                &[&run_id, &"exhausted", &MAX_RESTART_RESETS],
            )
            .await
            .expect("seed resets");

        let launcher = RecordingLauncher::default();
        sweep_orphaned_runs(&pg, &me, &HashSet::new(), &|_| false, &launcher)
            .await
            .expect("sweep");
        assert_eq!(launcher.0.lock().unwrap().len(), 1);

        let rows = pg.list_subtasks(run_id).await.expect("list");
        let l = rows.iter().find(|s| s.task_id == "lost").unwrap();
        assert_eq!(l.state, SubtaskState::Submitted);
        assert_eq!(l.task_run_id, None, "the dead worker's id is cleared");
        assert_eq!(l.restart_resets, 1);
        let r = rows.iter().find(|s| s.task_id == "reported").unwrap();
        assert_eq!(r.state, SubtaskState::Completed);
        assert_eq!(r.restart_resets, 0);
        let x = rows.iter().find(|s| s.task_id == "exhausted").unwrap();
        assert_eq!(x.state, SubtaskState::Failed);

        // A whole-row upsert (what dispatch does) must not reset the count.
        let mut redispatched = l.clone();
        redispatched.state = SubtaskState::Working;
        redispatched.task_run_id = Some(Uuid::new_v4());
        pg.upsert_subtask(&redispatched).await.expect("re-dispatch");
        let again = pg.list_subtasks(run_id).await.expect("list");
        assert_eq!(
            again
                .iter()
                .find(|s| s.task_id == "lost")
                .unwrap()
                .restart_resets,
            1
        );

        // The settlement write is guarded on the row still carrying the dead
        // worker's id: a row that moved on (here: re-dispatched under a new
        // worker) is not clobbered.
        let moved = pg
            .settle_lost_worker(
                run_id,
                "lost",
                lost.task_run_id,
                LostWorkerSettlement::Resubmit,
            )
            .await
            .expect("guarded settle");
        assert!(!moved, "a stale task_run_id must not move the row");
        let after = pg.list_subtasks(run_id).await.expect("list");
        let row = after.iter().find(|s| s.task_id == "lost").unwrap();
        assert_eq!(row.state, SubtaskState::Working);
        assert_eq!(row.task_run_id, redispatched.task_run_id);

        cleanup(&pg, &[run_id]).await;
    }

    #[tokio::test]
    #[ignore = "needs PG fixture (DATABASE_URL); orchestration schema self-heals at PgDb::new"]
    async fn sweep_leaves_foreign_and_unowned_runs_untouched() {
        let pg = PgDb::new_for_test().await;
        let me = unique_owner();
        let other = unique_owner();
        let foreign = Uuid::new_v4();
        let unowned = Uuid::new_v4();
        pg.create_run(
            foreign,
            "someone else's",
            None,
            &["implement".to_string()],
            &OrchestrationRunConfig::default(),
            &other,
        )
        .await
        .expect("create foreign");
        pg.create_run(
            unowned,
            "pre-column",
            None,
            &["implement".to_string()],
            &OrchestrationRunConfig::default(),
            &other,
        )
        .await
        .expect("create unowned");
        let conn = pg.pool().get().await.expect("conn");
        conn.execute(
            "UPDATE orchestration.runs SET owner_instance = NULL WHERE run_id = $1",
            &[&unowned],
        )
        .await
        .expect("null the owner");
        for id in [foreign, unowned] {
            pg.upsert_subtask(&working(id, "w", false, 0))
                .await
                .expect("upsert");
        }

        let launcher = RecordingLauncher::default();
        let report = sweep_orphaned_runs(&pg, &me, &HashSet::new(), &|_| false, &launcher)
            .await
            .expect("sweep");
        let launched: Vec<Uuid> = launcher
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        assert!(!launched.contains(&foreign) && !launched.contains(&unowned));
        assert!(report
            .skipped
            .contains(&(foreign, SweepSkip::ForeignOwner(other.clone()))));
        assert!(report.skipped.contains(&(unowned, SweepSkip::Unowned)));
        for id in [foreign, unowned] {
            let rows = pg.list_subtasks(id).await.expect("list");
            assert_eq!(rows[0].state, SubtaskState::Working, "not settled");
            assert_eq!(rows[0].restart_resets, 0);
            let run = pg.get_run(id).await.expect("get").expect("row");
            assert_eq!(run.status, "running");
        }
        assert_eq!(
            pg.get_run(unowned).await.unwrap().unwrap().owner_instance,
            None,
            "the sweep never adopts an unowned row"
        );

        cleanup(&pg, &[foreign, unowned]).await;
    }
}
