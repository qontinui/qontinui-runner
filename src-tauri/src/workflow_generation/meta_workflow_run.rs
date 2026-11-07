//! Running a spec-authoring meta-workflow: the flywheel's execute path.
//!
//! Plan `2026-11-06-what-the-fleet-records-and-advertises-matches-what-actually-ran`,
//! Phase 3. `spec_authoring` builds a meta-workflow and hands it to a
//! [`MetaWorkflowExecutor`](super::spec_authoring::MetaWorkflowExecutor). The
//! production executor used to persist the workflow, insert a task-run row as
//! `running` and poll that row for 120 s. Nothing ever executed the row, so
//! every execute that got that far timed out into `authoring-failed` and left a
//! `running` orphan, which a runner restart could then resume
//! (`startup_resume.rs`) outside any authorization.
//!
//! [`execute_meta_workflow_blocking`] now does three things, in order:
//!
//! 1. **It asks first, before anything is written.** One call,
//!    [`authorize_spawn`](crate::agent_authorization::authorize_spawn) for the
//!    `standing_continuation` class under the caller's [`SpawnOrigin`], answers
//!    both questions: it runs coord's device-drain gate before it reads the
//!    agent registry. A drained device (or an unknown drain state) DEFERS an
//!    autonomous execute, and a registry refusal makes it NOT AUTHORIZED.
//!    Neither persists anything, and neither is an authoring failure: the
//!    caller puts the proposal back in the queue. A meta-workflow outlives the
//!    request that started it, which is what `standing_continuation` covers.
//! 2. **It starts the run through the in-process spawn door.** It persists the
//!    meta-workflow, then calls
//!    [`call_door_in_process`](crate::commands::operator_doors::call_door_in_process)
//!    on `/unified-workflows/{id}/run`. That door core is the one the runner UI
//!    and the scheduler start workflows through, so the run gets the same
//!    launch as any other workflow run (DAG driver, Restate or the loop
//!    controller). The task run polled below is the one the door created, and
//!    the door's own drain gate judges the same origin again.
//! 3. **It waits for that run, then stops it if it overstays.** It polls until
//!    the run is terminal, for at most [`EXECUTE_TIMEOUT_ENV`] seconds (default
//!    one hour). A run still going at the deadline is STOPPED. So is a run
//!    whose caller went away mid-poll, which happens when an HTTP client hangs
//!    up and drops this future. "Stopped" means the row reads `stopped`, which
//!    the loop controller honours at its next check. A meta-workflow spends the
//!    user's AI account, and nothing may keep spending it once no one is
//!    waiting for its output.
//!
//! **What this does not fix (coord finding `ff957bb7`).** The meta-workflow
//! that `spec_authoring` builds is the runner's WORKFLOW-generation template,
//! and the output read here is `task_runs.output_log`. So a run that completes
//! still gives `parse_meta_workflow_output` no `IrPageSpec`. Running the task
//! run is necessary for a candidate. It is not sufficient.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use crate::agent_authorization::{DrainAdmission, SpawnDecision, SpawnPath};
use crate::commands::operator_doors::DoorReply;
use crate::coord_drain_state::{DeferClass, SpawnOrigin};
use crate::database::pg::PgDb;

/// The key the draining banner counts a deferred execute under.
pub(crate) const SPEC_EXECUTE_WORK_KEY: &str = "spec_flywheel:execute";

/// Env knob: the longest an execute waits for its run, in whole seconds.
pub(crate) const EXECUTE_TIMEOUT_ENV: &str = "QONTINUI_SPEC_EXECUTE_TIMEOUT_SECS";

/// The default for [`EXECUTE_TIMEOUT_ENV`]: one hour.
pub(crate) const DEFAULT_EXECUTE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// How often the run's row is read.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The `stopped` reason written when a run is still going at its deadline.
pub(crate) const DEADLINE_STOP_REASON: &str = "spec execute: past its deadline, nobody waiting";

/// The `stopped` reason written when the execute waiting on a run went away.
pub(crate) const ABANDONED_STOP_REASON: &str = "spec execute: abandoned by its caller";

/// Why a meta-workflow run returned no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaWorkflowError {
    /// Coord has drained this device, or its drain state is unknown, and the
    /// execute is autonomous. Not a failure: the work waits for the drain to
    /// lift. `code` is coord's 409 vocabulary ([`DeferClass::code`]).
    Deferred { code: &'static str, reason: String },
    /// The agent registry refused the execute's `standing_continuation` spawn.
    NotAuthorized(String),
    /// The run could not be started, failed, was stopped, or overran its
    /// deadline.
    Failed(String),
}

impl std::fmt::Display for MetaWorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetaWorkflowError::Deferred { code, reason } => write!(f, "{code}: {reason}"),
            MetaWorkflowError::NotAuthorized(reason) => write!(f, "not authorized: {reason}"),
            MetaWorkflowError::Failed(reason) => f.write_str(reason),
        }
    }
}

/// PURE: whether an execute may spawn, given the authorization decision.
/// `Warn` and `SerializeToBound` spawn; a drain deferral and every refusal do
/// not.
pub(crate) fn admission(decision: &SpawnDecision) -> Result<(), MetaWorkflowError> {
    if decision.allows_spawn() {
        return Ok(());
    }
    let reason = decision
        .reason()
        .unwrap_or("no reason recorded")
        .to_string();
    match decision.drain_class() {
        Some(class) => Err(MetaWorkflowError::Deferred {
            code: class.code(),
            reason,
        }),
        None => Err(MetaWorkflowError::NotAuthorized(format!(
            "{}: {reason}",
            decision.label()
        ))),
    }
}

/// PURE: the in-process spawn door that runs persisted workflow `workflow_id`.
pub(crate) fn run_door_path(workflow_id: &str) -> String {
    format!("/unified-workflows/{workflow_id}/run")
}

/// PURE: the run door's request body. Always a fresh execution: an execute
/// never resumes an earlier run of the same meta-workflow.
pub(crate) fn run_door_body() -> serde_json::Value {
    serde_json::json!({ "force_fresh_start": true })
}

/// PURE: the task run the run door started, or why there is none. The door
/// answers `ApiResponse { data: { task_run_id } }` on 200. A 409 whose `code`
/// is a drain code is the door's own drain gate deferring the run, which can
/// happen when the drain changes between [`admission`] and the door call.
pub(crate) fn started_task_run(reply: &DoorReply) -> Result<String, MetaWorkflowError> {
    if reply.status == 200 {
        return reply
            .body
            .get("data")
            .and_then(|d| d.get("task_run_id"))
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                MetaWorkflowError::Failed(format!(
                    "the run door answered 200 without a task_run_id: {}",
                    reply.body
                ))
            });
    }
    let message = reply
        .body
        .get("error")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| reply.body.to_string());
    if reply.status == 409 {
        let code = reply.body.get("code").and_then(|v| v.as_str());
        if let Some(class) = [DeferClass::Drained, DeferClass::Unknown]
            .into_iter()
            .find(|c| Some(c.code()) == code)
        {
            return Err(MetaWorkflowError::Deferred {
                code: class.code(),
                reason: message,
            });
        }
    }
    Err(MetaWorkflowError::Failed(format!(
        "the run door answered {}: {message}",
        reply.status
    )))
}

/// PURE: the deadline an execute waits, from [`EXECUTE_TIMEOUT_ENV`]'s raw
/// value. Unset, unparseable or zero means [`DEFAULT_EXECUTE_TIMEOUT`].
pub(crate) fn execute_timeout(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_EXECUTE_TIMEOUT)
}

/// Where a task run stands, as the poll reads its `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    Running,
    Complete,
    Failed,
    Stopped,
}

/// PURE: a task-run `status` as a [`RunState`]. The loop controller finishes a
/// run as `complete` (`complete_task_run`) and the DAG driver as `completed`.
/// Anything not terminal, `paused` included, is still running: the deadline
/// bounds the wait.
pub(crate) fn run_state(status: &str) -> RunState {
    match status {
        "complete" | "completed" => RunState::Complete,
        "failed" => RunState::Failed,
        "stopped" => RunState::Stopped,
        _ => RunState::Running,
    }
}

/// PURE: a finished run's output. `output_log` parsed as JSON when it is JSON,
/// otherwise wrapped as `{"output_log": <text>}` so the output parsers can look
/// for their envelope keys.
pub(crate) fn run_output(output_log: &str) -> serde_json::Value {
    serde_json::from_str(output_log)
        .unwrap_or_else(|_| serde_json::json!({ "output_log": output_log }))
}

/// A task-run row, as the poll reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunRow {
    pub status: String,
    pub error_message: Option<String>,
    pub output_log: String,
}

/// Where the poll reads a task run and how it stops one: [`PgDb`] in
/// production, a scripted ledger in tests.
#[async_trait::async_trait]
pub(crate) trait RunLedger: Send + Sync {
    async fn read(&self, task_run_id: &str) -> Result<Option<RunRow>, String>;
    async fn stop(&self, task_run_id: &str, reason: &str) -> Result<(), String>;
}

#[async_trait::async_trait]
impl RunLedger for PgDb {
    async fn read(&self, task_run_id: &str) -> Result<Option<RunRow>, String> {
        Ok(self.get_task_run(task_run_id).await?.map(|t| RunRow {
            status: t.status,
            error_message: t.error_message,
            output_log: t.output_log,
        }))
    }

    async fn stop(&self, task_run_id: &str, reason: &str) -> Result<(), String> {
        self.stop_task_run(task_run_id, reason).await
    }
}

/// Stops the run it guards if it is dropped while still armed: an execute
/// whose caller went away must not leave its run spending the user's AI
/// account with nobody waiting for the output.
struct StopOnDrop {
    ledger: Arc<dyn RunLedger>,
    task_run_id: String,
    armed: bool,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let task_run_id = std::mem::take(&mut self.task_run_id);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                task_run_id = %task_run_id,
                "spec execute abandoned outside a tokio runtime; its run was NOT stopped"
            );
            return;
        };
        let ledger = self.ledger.clone();
        handle.spawn(async move {
            match ledger.stop(&task_run_id, ABANDONED_STOP_REASON).await {
                Ok(()) => info!(task_run_id = %task_run_id, "spec execute abandoned; run stopped"),
                Err(e) => warn!(
                    task_run_id = %task_run_id,
                    "spec execute abandoned; stopping its run failed: {e}"
                ),
            }
        });
    }
}

/// Wait for `task_run_id` to reach a terminal state, reading it every
/// `interval`, for at most `timeout`. The run is stopped if it is still going
/// at the deadline, and also if this future is dropped first.
///
/// A failed read is retried until the deadline: one PG hiccup is no reason to
/// abandon a run that is still working. A row that has vanished is a failure
/// with nothing left to stop.
pub(crate) async fn await_run_output(
    ledger: Arc<dyn RunLedger>,
    task_run_id: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<serde_json::Value, MetaWorkflowError> {
    let mut guard = StopOnDrop {
        ledger: ledger.clone(),
        task_run_id: task_run_id.to_string(),
        armed: true,
    };
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            guard.armed = false;
            if let Err(e) = ledger.stop(task_run_id, DEADLINE_STOP_REASON).await {
                warn!(
                    task_run_id,
                    "stopping an overdue spec execute run failed: {e}"
                );
            }
            return Err(MetaWorkflowError::Failed(format!(
                "meta-workflow task run {task_run_id} was still running after {}s and was stopped",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(interval.min(deadline - now)).await;
        let row = match ledger.read(task_run_id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                guard.armed = false;
                return Err(MetaWorkflowError::Failed(format!(
                    "meta-workflow task run {task_run_id} disappeared"
                )));
            }
            Err(e) => {
                warn!(
                    task_run_id,
                    "reading a spec execute run failed, retrying: {e}"
                );
                continue;
            }
        };
        match run_state(&row.status) {
            RunState::Running => continue,
            RunState::Complete => {
                guard.armed = false;
                return Ok(run_output(&row.output_log));
            }
            RunState::Failed => {
                guard.armed = false;
                return Err(MetaWorkflowError::Failed(row.error_message.unwrap_or_else(
                    || "meta-workflow failed (no error message)".to_string(),
                )));
            }
            RunState::Stopped => {
                guard.armed = false;
                return Err(MetaWorkflowError::Failed(format!(
                    "meta-workflow task run {task_run_id} was stopped: {}",
                    row.error_message
                        .unwrap_or_else(|| "no reason recorded".to_string())
                )));
            }
        }
    }
}

/// Run `meta_wf` on behalf of `origin` and return its output. See the module
/// docs for the three steps.
///
/// Listed in `runner_spawn_sites.txt` as an autonomous spawn site: it calls the
/// in-process spawn door, and it branches on `authorize_spawn` before anything
/// else.
pub(crate) async fn execute_meta_workflow_blocking(
    pg_db: Arc<PgDb>,
    meta_wf: crate::unified_workflows::UnifiedWorkflow,
    origin: SpawnOrigin,
) -> Result<serde_json::Value, MetaWorkflowError> {
    let decision = crate::agent_authorization::authorize_spawn(
        None,
        SpawnPath::StandingContinuation,
        DrainAdmission::work(origin, SPEC_EXECUTE_WORK_KEY),
    )
    .await;
    admission(&decision)?;
    if let SpawnDecision::Warn { reason } = &decision {
        warn!("spec execute: spawning under a warn_proceed disposition: {reason}");
    }

    let mut create_request = crate::unified_workflows::unified_workflow_to_create_request(&meta_wf);
    create_request.generated_by_task_run_id = None;
    let saved = pg_db
        .create_unified_workflow(&create_request)
        .await
        .map_err(|e| MetaWorkflowError::Failed(format!("create_unified_workflow: {e}")))?;

    let reply = crate::commands::operator_doors::call_door_in_process(
        &run_door_path(&saved.id),
        run_door_body(),
        origin,
    )
    .await
    .map_err(MetaWorkflowError::Failed)?;
    let task_run_id = started_task_run(&reply)?;
    info!(
        meta_workflow_id = %saved.id,
        task_run_id = %task_run_id,
        origin = %origin,
        "spec execute: meta-workflow run started"
    );

    let timeout = execute_timeout(std::env::var(EXECUTE_TIMEOUT_ENV).ok().as_deref());
    await_run_output(pg_db, &task_run_id, timeout, POLL_INTERVAL).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // -----------------------------------------------------------------
    // admission — every SpawnDecision arm
    // -----------------------------------------------------------------

    #[test]
    fn admission_spawns_on_allow_warn_and_serialize_to_bound() {
        assert_eq!(admission(&SpawnDecision::Allow), Ok(()));
        assert_eq!(
            admission(&SpawnDecision::Warn {
                reason: "warn_proceed".into()
            }),
            Ok(())
        );
        assert_eq!(
            admission(&SpawnDecision::SerializeToBound {
                bound: 1,
                in_flight: 1
            }),
            Ok(())
        );
    }

    #[test]
    fn admission_defers_a_drained_or_unreadable_device_with_its_code() {
        assert_eq!(
            admission(&SpawnDecision::DeferredByDrain {
                reason: "coord has drained this device".into(),
                class: DeferClass::Drained,
            }),
            Err(MetaWorkflowError::Deferred {
                code: "device_drained",
                reason: "coord has drained this device".into(),
            })
        );
        assert_eq!(
            admission(&SpawnDecision::DeferredByDrain {
                reason: "drain state unknown".into(),
                class: DeferClass::Unknown,
            }),
            Err(MetaWorkflowError::Deferred {
                code: "drain_unreadable",
                reason: "drain state unknown".into(),
            })
        );
    }

    #[test]
    fn admission_refuses_deny_and_degrade_as_not_authorized() {
        assert_eq!(
            admission(&SpawnDecision::Deny {
                reason: "standing_continuation disabled".into()
            }),
            Err(MetaWorkflowError::NotAuthorized(
                "deny: standing_continuation disabled".into()
            ))
        );
        assert_eq!(
            admission(&SpawnDecision::DegradeToInline {
                reason: "degrade".into()
            }),
            Err(MetaWorkflowError::NotAuthorized(
                "degrade_to_inline: degrade".into()
            ))
        );
    }

    /// The production entry, end to end, on its refusal arm. Nothing in a test
    /// process reads coord's drain, so its state is the boot `Unknown`, which
    /// defers every autonomous spawn. The execute must answer `Deferred` before
    /// it touches the database: this `PgDb` fails every query, so an execute
    /// that persisted (or started) anything before asking would answer
    /// `Failed` instead.
    #[tokio::test]
    async fn an_execute_the_drain_defers_persists_and_starts_nothing() {
        assert!(
            matches!(
                crate::coord_drain_state::current(),
                crate::coord_drain_state::CoordDrainState::Unknown { .. }
            ),
            "precondition: no test folds a drain observation into this process, so \
             its drain state is the boot Unknown"
        );
        let request = crate::workflow_generation::GenerateWorkflowRequest {
            description: "Spec-authoring for a test page".to_string(),
            category: Some("spec-authoring".to_string()),
            ..Default::default()
        };
        let meta_wf = crate::workflow_generation::meta_workflow::build_meta_workflow_template(
            &request, "", None, None,
        );
        let out = execute_meta_workflow_blocking(
            PgDb::new_noop_for_test(),
            meta_wf,
            SpawnOrigin::Unknown,
        )
        .await;
        match out {
            Err(MetaWorkflowError::Deferred { code, .. }) => {
                assert_eq!(code, "drain_unreadable")
            }
            other => panic!(
                "an autonomous execute on a device whose drain state is unknown must be \
                 deferred before it persists or starts anything; got {other:?}"
            ),
        }
    }

    /// The gate comes first: nothing is persisted and no run starts until
    /// `authorize_spawn` has answered and `admission` has let the execute
    /// through. `runner_spawn_sites.txt`'s guard proves the gate is CALLED and
    /// branched on; this proves it comes BEFORE the work it guards, which that
    /// guard cannot see.
    #[test]
    fn the_execute_authorizes_before_it_writes_or_spawns() {
        let src = include_str!("meta_workflow_run.rs");
        let (_, rest) = src
            .split_once("pub(crate) async fn execute_meta_workflow_blocking(")
            .expect("execute_meta_workflow_blocking is defined");
        let (body, _) = rest.split_once("\n}\n").expect("the fn body ends");
        let at = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("`{needle}` is missing from the execute"))
        };
        let gate = at("authorize_spawn(");
        let admit = at("admission(&decision)?");
        let persist = at("create_unified_workflow(");
        let start = at("call_door_in_process(");
        assert!(
            gate < admit && admit < persist && persist < start,
            "order must be authorize_spawn -> admission -> persist -> start \
             (found {gate}, {admit}, {persist}, {start})"
        );
    }

    // -----------------------------------------------------------------
    // the run door's request and reply
    // -----------------------------------------------------------------

    #[test]
    fn the_run_door_is_the_unified_workflow_run_path_and_starts_fresh() {
        assert_eq!(run_door_path("wf-1"), "/unified-workflows/wf-1/run");
        assert_eq!(run_door_body()["force_fresh_start"], true);
    }

    fn reply(status: u16, body: serde_json::Value) -> DoorReply {
        DoorReply { status, body }
    }

    #[test]
    fn a_200_reply_yields_the_task_run_the_door_created() {
        let ok = reply(
            200,
            serde_json::json!({ "success": true, "data": { "task_run_id": "unified-workflow-wf-1-7" } }),
        );
        assert_eq!(
            started_task_run(&ok),
            Ok("unified-workflow-wf-1-7".to_string())
        );
    }

    #[test]
    fn a_200_reply_without_a_task_run_id_is_a_failure() {
        for body in [
            serde_json::json!({ "success": true }),
            serde_json::json!({ "success": true, "data": { "task_run_id": "" } }),
            serde_json::json!({ "success": true, "data": { "task_run_id": 7 } }),
        ] {
            assert!(
                matches!(
                    started_task_run(&reply(200, body.clone())),
                    Err(MetaWorkflowError::Failed(_))
                ),
                "{body}"
            );
        }
    }

    #[test]
    fn a_409_with_a_drain_code_is_the_doors_own_deferral() {
        for code in ["device_drained", "drain_unreadable"] {
            let refused = reply(
                409,
                serde_json::json!({ "success": false, "error": "deferred", "code": code }),
            );
            assert_eq!(
                started_task_run(&refused),
                Err(MetaWorkflowError::Deferred {
                    code,
                    reason: "deferred".into()
                })
            );
        }
    }

    #[test]
    fn any_other_refusal_is_a_failure_naming_the_status() {
        let other_409 = reply(
            409,
            serde_json::json!({ "success": false, "error": "busy", "code": "something_else" }),
        );
        assert_eq!(
            started_task_run(&other_409),
            Err(MetaWorkflowError::Failed(
                "the run door answered 409: busy".into()
            ))
        );
        let not_found = reply(
            404,
            serde_json::json!({ "success": false, "error": "Unified workflow not found: wf-1" }),
        );
        assert_eq!(
            started_task_run(&not_found),
            Err(MetaWorkflowError::Failed(
                "the run door answered 404: Unified workflow not found: wf-1".into()
            ))
        );
    }

    // -----------------------------------------------------------------
    // the knobs and the readers
    // -----------------------------------------------------------------

    #[test]
    fn execute_timeout_reads_whole_seconds_and_defaults_otherwise() {
        assert_eq!(execute_timeout(Some("120")), Duration::from_secs(120));
        assert_eq!(execute_timeout(Some(" 90 ")), Duration::from_secs(90));
        for raw in [None, Some("0"), Some("-5"), Some("an hour"), Some("")] {
            assert_eq!(execute_timeout(raw), DEFAULT_EXECUTE_TIMEOUT, "{raw:?}");
        }
    }

    #[test]
    fn run_state_names_every_terminal_status_and_waits_on_the_rest() {
        assert_eq!(run_state("complete"), RunState::Complete);
        assert_eq!(run_state("completed"), RunState::Complete);
        assert_eq!(run_state("failed"), RunState::Failed);
        assert_eq!(run_state("stopped"), RunState::Stopped);
        for status in ["running", "paused", "cancelling", "pending", ""] {
            assert_eq!(run_state(status), RunState::Running, "{status:?}");
        }
    }

    #[test]
    fn run_output_parses_json_and_wraps_anything_else() {
        assert_eq!(
            run_output(r#"{"id":"x"}"#),
            serde_json::json!({ "id": "x" })
        );
        assert_eq!(
            run_output("AI said hello"),
            serde_json::json!({ "output_log": "AI said hello" })
        );
    }

    // -----------------------------------------------------------------
    // await_run_output — a scripted ledger
    // -----------------------------------------------------------------

    /// Answers `read` from a script (the last answer repeats once the script
    /// runs out) and records every `stop`.
    struct ScriptedLedger {
        reads: Mutex<VecDeque<Result<Option<RunRow>, String>>>,
        stops: Mutex<Vec<(String, String)>>,
    }

    impl ScriptedLedger {
        fn new(reads: Vec<Result<Option<RunRow>, String>>) -> Arc<Self> {
            Arc::new(Self {
                reads: Mutex::new(reads.into()),
                stops: Mutex::new(Vec::new()),
            })
        }

        fn stops(&self) -> Vec<(String, String)> {
            self.stops.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl RunLedger for ScriptedLedger {
        async fn read(&self, _task_run_id: &str) -> Result<Option<RunRow>, String> {
            let mut reads = self.reads.lock().unwrap();
            if reads.len() > 1 {
                reads.pop_front().unwrap()
            } else {
                reads.front().cloned().expect("a scripted read")
            }
        }

        async fn stop(&self, task_run_id: &str, reason: &str) -> Result<(), String> {
            self.stops
                .lock()
                .unwrap()
                .push((task_run_id.to_string(), reason.to_string()));
            Ok(())
        }
    }

    fn row(status: &str, error: Option<&str>, output_log: &str) -> Result<Option<RunRow>, String> {
        Ok(Some(RunRow {
            status: status.into(),
            error_message: error.map(str::to_string),
            output_log: output_log.into(),
        }))
    }

    const TIMEOUT: Duration = Duration::from_secs(60);
    const EVERY: Duration = Duration::from_secs(5);

    #[tokio::test(start_paused = true)]
    async fn a_completed_run_returns_its_output_and_is_not_stopped() {
        let ledger = ScriptedLedger::new(vec![
            row("running", None, ""),
            row("running", None, ""),
            row("complete", None, r#"{"id":"page"}"#),
        ]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(out, Ok(serde_json::json!({ "id": "page" })));
        tokio::task::yield_now().await;
        assert!(ledger.stops().is_empty(), "a finished run is never stopped");
    }

    #[tokio::test(start_paused = true)]
    async fn a_dag_completed_run_counts_as_finished() {
        let ledger = ScriptedLedger::new(vec![row("completed", None, "plain text")]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(out, Ok(serde_json::json!({ "output_log": "plain text" })));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_run_returns_its_error_message() {
        let ledger = ScriptedLedger::new(vec![row("failed", Some("builder step failed"), "")]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed("builder step failed".into()))
        );
        tokio::task::yield_now().await;
        assert!(ledger.stops().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_run_is_a_failure_naming_the_stop() {
        let ledger = ScriptedLedger::new(vec![row("stopped", Some("User stopped"), "")]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 was stopped: User stopped".into()
            ))
        );
        tokio::task::yield_now().await;
        assert!(
            ledger.stops().is_empty(),
            "already stopped; not stopped again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_still_going_at_the_deadline_is_stopped() {
        let ledger = ScriptedLedger::new(vec![row("running", None, "")]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 was still running after 60s and was stopped".into()
            ))
        );
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-1".to_string(), DEADLINE_STOP_REASON.to_string())],
            "stopped exactly once, by the deadline, not again by the guard"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_vanished_run_fails_with_nothing_to_stop() {
        let ledger = ScriptedLedger::new(vec![Ok(None)]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 disappeared".into()
            ))
        );
        tokio::task::yield_now().await;
        assert!(ledger.stops().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_keeps_waiting_for_the_run() {
        let ledger = ScriptedLedger::new(vec![
            Err("PG pool error: timed out".into()),
            row("complete", None, r#"{"ok":true}"#),
        ]);
        let out = await_run_output(ledger.clone(), "tr-1", TIMEOUT, EVERY).await;
        assert_eq!(out, Ok(serde_json::json!({ "ok": true })));
    }

    #[tokio::test(start_paused = true)]
    async fn an_execute_dropped_mid_poll_stops_its_run() {
        let ledger = ScriptedLedger::new(vec![row("running", None, "")]);
        let poll = await_run_output(ledger.clone(), "tr-9", TIMEOUT, EVERY);
        // Let the poll read the run at least once, then abandon it the way an
        // HTTP client hanging up drops its handler future.
        let raced = tokio::time::timeout(Duration::from_secs(12), poll).await;
        assert!(raced.is_err(), "the poll was still waiting when dropped");
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-9".to_string(), ABANDONED_STOP_REASON.to_string())]
        );
    }
}
