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
//! [`execute_meta_workflow_blocking`] now does four things, in order:
//!
//! 1. **It asks first, before anything is written.** One call,
//!    [`authorize_spawn`](crate::agent_authorization::authorize_spawn) for the
//!    `standing_continuation` class under the caller's [`SpawnOrigin`], answers
//!    both questions: it runs coord's device-drain gate before it reads the
//!    agent registry. A drained device (or an unknown drain state, or a
//!    dispatch role that closes the agent lane) DEFERS an autonomous execute,
//!    and a registry refusal makes it NOT AUTHORIZED.
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
//!    the door's own drain gate judges the same origin again. The door call
//!    runs in a task of its own, which hands the started run over already
//!    guarded ([`StopOnDrop`]): an execute dropped while the door call is in
//!    flight stops the run the door then starts. A door that starts nothing
//!    leaves nothing behind: the persisted meta-workflow is deleted again.
//! 3. **It binds the run to the execute.** It records the run on the
//!    proposal's claim, so a scan that later finds the claim stale stops the
//!    run, and it turns the run's `auto_continue` off, so a runner restart
//!    records the interrupted run `failed` instead of resuming it outside this
//!    execute's authorization with nobody reading its output. Both are retried
//!    at every poll until they land. A claim lost before the run is recorded on
//!    it (a scan took the proposal back) stops the run at once.
//! 4. **It waits for that run, then stops it if it overstays.** It polls the
//!    run's status until the run has finished, for at most
//!    [`EXECUTE_TIMEOUT_ENV`] seconds (default one hour), and reads the run's
//!    output once, when it has. A run still going at the deadline is marked
//!    STOPPED. So is a run whose caller went away mid-poll, which happens when
//!    an HTTP client hangs up and drops this future. A stop never touches a
//!    run that has already finished. "Stopped" means the row reads `stopped`:
//!    the loop controller and Restate honour that at their next status check,
//!    but nothing kills the `claude` process, and the DAG driver writes its
//!    own final status over it when it ends. A meta-workflow spends the user's
//!    AI account, and nothing may keep spending it once no one is waiting for
//!    its output.
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

/// The `stopped` reason written when the execute's claim on its proposal was
/// lost before its run could be recorded on it: a scan put the proposal back
/// in the queue, so this run's outcome would be written nowhere.
pub(crate) const CLAIM_LOST_STOP_REASON: &str =
    "spec execute: its proposal claim was lost, nobody will record the outcome";

/// The proposal an execute's run belongs to, and the claim token the execute
/// holds on it (`PgDb::claim_proposal_for_execute`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunClaim {
    pub proposal_id: String,
    pub token: String,
}

/// Why a meta-workflow run returned no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaWorkflowError {
    /// Coord has drained this device, its drain state is unknown, or its
    /// dispatch role closes the agent lane, and the execute is autonomous. Not
    /// a failure: the work waits for the drain or the role to lift. `code` is
    /// coord's 409 vocabulary ([`DeferClass::code`]).
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
        if let Some(class) = reply
            .body
            .get("code")
            .and_then(|v| v.as_str())
            .and_then(DeferClass::from_code)
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

/// The deadline this process's executes wait under: [`EXECUTE_TIMEOUT_ENV`],
/// read now.
pub(crate) fn configured_execute_timeout() -> Duration {
    execute_timeout(std::env::var(EXECUTE_TIMEOUT_ENV).ok().as_deref())
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
/// Anything not finished, `paused` included, is still running: the deadline
/// bounds the wait. The finished statuses are exactly the ones
/// `stop_task_run_if_unfinished` leaves alone (`FINISHED_TASK_RUN_STATUSES`).
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

/// A task run's status, as the poll reads it. Its output log is not part of
/// it: that is read once, when the run has finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunStatus {
    pub status: String,
    pub error_message: Option<String>,
}

/// Whether an execute's run is now recorded on the proposal claim it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimRecord {
    /// Recorded: a scan that later finds the claim stale stops this run.
    Recorded,
    /// The claim is no longer held: a scan put the proposal back in the queue,
    /// so nothing will record this run's outcome.
    Lost,
}

/// Where the poll reads a task run, binds it to its execute, and stops it:
/// [`PgDb`] in production, a scripted ledger in tests.
#[async_trait::async_trait]
pub(crate) trait RunLedger: Send + Sync {
    /// The run's status, or `None` when its row is gone.
    async fn status(&self, task_run_id: &str) -> Result<Option<RunStatus>, String>;
    /// The run's output log, or `None` when its row is gone.
    async fn output(&self, task_run_id: &str) -> Result<Option<String>, String>;
    /// Mark the run `stopped`, unless it has already finished. `Ok(false)`
    /// means it had, and its row was left as it was.
    async fn stop_if_unfinished(&self, task_run_id: &str, reason: &str) -> Result<bool, String>;
    /// Record the run on the proposal claim its execute holds.
    async fn record_on_claim(
        &self,
        claim: &RunClaim,
        task_run_id: &str,
    ) -> Result<ClaimRecord, String>;
    /// Turn the run's `auto_continue` off. A runner restart then records the
    /// interrupted run `failed` (`startup_resume.rs`) instead of resuming it
    /// with nobody waiting for its output. No executor reads `auto_continue`
    /// while a run is going (startup resume, and chat resume for chat rows,
    /// act on it only at boot; the task-run API and GraphQL only report it),
    /// so the running run is unaffected.
    async fn disable_resume(&self, task_run_id: &str) -> Result<(), String>;
}

#[async_trait::async_trait]
impl RunLedger for PgDb {
    async fn status(&self, task_run_id: &str) -> Result<Option<RunStatus>, String> {
        Ok(self
            .get_task_run_status(task_run_id)
            .await?
            .map(|(status, error_message)| RunStatus {
                status,
                error_message,
            }))
    }

    async fn output(&self, task_run_id: &str) -> Result<Option<String>, String> {
        self.get_task_output_opt(task_run_id).await
    }

    async fn stop_if_unfinished(&self, task_run_id: &str, reason: &str) -> Result<bool, String> {
        self.stop_task_run_if_unfinished(task_run_id, reason).await
    }

    async fn record_on_claim(
        &self,
        claim: &RunClaim,
        task_run_id: &str,
    ) -> Result<ClaimRecord, String> {
        let held = self
            .record_execute_run(&claim.proposal_id, &claim.token, task_run_id)
            .await?;
        Ok(if held {
            ClaimRecord::Recorded
        } else {
            ClaimRecord::Lost
        })
    }

    async fn disable_resume(&self, task_run_id: &str) -> Result<(), String> {
        self.set_task_auto_continue(task_run_id, false).await
    }
}

/// Stops the run it guards if it is dropped while still armed: an execute
/// whose caller went away must not leave its run spending the user's AI
/// account with nobody waiting for the output. A run that finished before the
/// stop reached it is left as it is.
pub(crate) struct StopOnDrop {
    ledger: Arc<dyn RunLedger>,
    task_run_id: String,
    armed: bool,
}

impl StopOnDrop {
    /// Guard `task_run_id`, armed.
    pub(crate) fn armed(ledger: Arc<dyn RunLedger>, task_run_id: String) -> Self {
        Self {
            ledger,
            task_run_id,
            armed: true,
        }
    }

    /// The run this guard watches.
    pub(crate) fn task_run_id(&self) -> &str {
        &self.task_run_id
    }
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
            match ledger
                .stop_if_unfinished(&task_run_id, ABANDONED_STOP_REASON)
                .await
            {
                Ok(true) => {
                    info!(task_run_id = %task_run_id, "spec execute abandoned; run stopped")
                }
                Ok(false) => info!(
                    task_run_id = %task_run_id,
                    "spec execute abandoned after its run had finished; run left as it was"
                ),
                Err(e) => warn!(
                    task_run_id = %task_run_id,
                    "spec execute abandoned; stopping its run failed: {e}"
                ),
            }
        });
    }
}

/// PURE: the error a run that finished without succeeding answers.
fn unsuccessful(
    task_run_id: &str,
    stopped: bool,
    error_message: Option<String>,
) -> MetaWorkflowError {
    if stopped {
        return MetaWorkflowError::Failed(format!(
            "meta-workflow task run {task_run_id} was stopped: {}",
            error_message.unwrap_or_else(|| "no reason recorded".to_string())
        ));
    }
    MetaWorkflowError::Failed(
        error_message.unwrap_or_else(|| "meta-workflow failed (no error message)".to_string()),
    )
}

/// PURE: the error for a run whose row is gone.
fn vanished(task_run_id: &str) -> MetaWorkflowError {
    MetaWorkflowError::Failed(format!("meta-workflow task run {task_run_id} disappeared"))
}

/// Wait for the run `guard` watches to finish, reading its status every
/// `interval`, for at most `timeout`. The run's output is read once, when it
/// has completed. The run is stopped if it is still going at the deadline, and
/// also if this future is dropped first (the guard's job).
///
/// Before each wait the run is bound to its execute until both writes have
/// landed: it is recorded on `claim`, and its `auto_continue` is turned off.
/// A claim found lost stops the run and fails the execute at once.
///
/// A failed read is retried until the deadline: one PG hiccup is no reason to
/// abandon a run that is still working, or to report a completed run's output
/// as empty. A row that has vanished is a failure with nothing left to stop.
pub(crate) async fn await_run_output(
    ledger: Arc<dyn RunLedger>,
    guard: StopOnDrop,
    claim: Option<&RunClaim>,
    timeout: Duration,
    interval: Duration,
) -> Result<serde_json::Value, MetaWorkflowError> {
    let mut guard = guard;
    let task_run_id = guard.task_run_id().to_string();
    let task_run_id = task_run_id.as_str();
    let mut recorded = claim.is_none();
    let mut resume_off = false;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let (false, Some(claim)) = (recorded, claim) {
            match ledger.record_on_claim(claim, task_run_id).await {
                Ok(ClaimRecord::Recorded) => recorded = true,
                Ok(ClaimRecord::Lost) => {
                    match ledger
                        .stop_if_unfinished(task_run_id, CLAIM_LOST_STOP_REASON)
                        .await
                    {
                        Ok(_) => guard.armed = false,
                        Err(e) => warn!(
                            task_run_id,
                            "stopping a spec execute run whose claim was lost failed \
                             (its guard tries again): {e}"
                        ),
                    }
                    return Err(MetaWorkflowError::Failed(format!(
                        "the claim on proposal {} was lost before meta-workflow task run \
                         {task_run_id} was recorded on it, so the run was stopped",
                        claim.proposal_id
                    )));
                }
                Err(e) => warn!(
                    task_run_id,
                    "recording a spec execute run on its proposal failed, retrying: {e}"
                ),
            }
        }
        if !resume_off {
            match ledger.disable_resume(task_run_id).await {
                Ok(()) => resume_off = true,
                Err(e) => warn!(
                    task_run_id,
                    "turning a spec execute run's auto_continue off failed, retrying: {e}"
                ),
            }
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            let (out, settled) = at_deadline(ledger.as_ref(), task_run_id, timeout).await;
            // A stop that failed leaves the guard armed: dropping it asks again.
            guard.armed = !settled;
            return out;
        }
        tokio::time::sleep(interval.min(deadline - now)).await;
        let status = match ledger.status(task_run_id).await {
            Ok(Some(status)) => status,
            Ok(None) => {
                guard.armed = false;
                return Err(vanished(task_run_id));
            }
            Err(e) => {
                warn!(
                    task_run_id,
                    "reading a spec execute run failed, retrying: {e}"
                );
                continue;
            }
        };
        match run_state(&status.status) {
            RunState::Running => continue,
            RunState::Complete => match ledger.output(task_run_id).await {
                Ok(Some(output_log)) => {
                    guard.armed = false;
                    return Ok(run_output(&output_log));
                }
                Ok(None) => {
                    guard.armed = false;
                    return Err(vanished(task_run_id));
                }
                Err(e) => {
                    warn!(
                        task_run_id,
                        "reading a finished spec execute run's output failed, retrying: {e}"
                    );
                    continue;
                }
            },
            RunState::Failed => {
                guard.armed = false;
                return Err(unsuccessful(task_run_id, false, status.error_message));
            }
            RunState::Stopped => {
                guard.armed = false;
                return Err(unsuccessful(task_run_id, true, status.error_message));
            }
        }
    }
}

/// The deadline has passed. Stop the run, unless it finished since it was
/// last read; then it answers what it finished with, read once. The `bool` is
/// whether the run is settled: stopped, or found finished. It is `false` only
/// when the stop itself failed, so the caller's guard stays armed and asks
/// again.
async fn at_deadline(
    ledger: &dyn RunLedger,
    task_run_id: &str,
    timeout: Duration,
) -> (Result<serde_json::Value, MetaWorkflowError>, bool) {
    let overdue = format!(
        "meta-workflow task run {task_run_id} was still running after {}s",
        timeout.as_secs()
    );
    match ledger
        .stop_if_unfinished(task_run_id, DEADLINE_STOP_REASON)
        .await
    {
        Ok(true) => (
            Err(MetaWorkflowError::Failed(format!(
                "{overdue} and was stopped"
            ))),
            true,
        ),
        Err(e) => {
            warn!(
                task_run_id,
                "stopping an overdue spec execute run failed: {e}"
            );
            (
                Err(MetaWorkflowError::Failed(format!(
                    "{overdue}; stopping it failed: {e}"
                ))),
                false,
            )
        }
        Ok(false) => (
            finished_at_deadline(ledger, task_run_id, &overdue).await,
            true,
        ),
    }
}

/// A run the deadline's stop found already finished: what it finished with,
/// read once.
async fn finished_at_deadline(
    ledger: &dyn RunLedger,
    task_run_id: &str,
    overdue: &str,
) -> Result<serde_json::Value, MetaWorkflowError> {
    let read_failed = |e: String| {
        MetaWorkflowError::Failed(format!(
            "meta-workflow task run {task_run_id} finished at its deadline, but reading it failed: {e}"
        ))
    };
    let status = ledger
        .status(task_run_id)
        .await
        .map_err(read_failed)?
        .ok_or_else(|| vanished(task_run_id))?;
    match run_state(&status.status) {
        RunState::Complete => ledger
            .output(task_run_id)
            .await
            .map_err(read_failed)?
            .map(|output_log| run_output(&output_log))
            .ok_or_else(|| vanished(task_run_id)),
        RunState::Failed => Err(unsuccessful(task_run_id, false, status.error_message)),
        RunState::Stopped => Err(unsuccessful(task_run_id, true, status.error_message)),
        RunState::Running => Err(MetaWorkflowError::Failed(format!(
            "{overdue}; the stop found it finished, yet it now reads `{}`",
            status.status
        ))),
    }
}

/// Why the run door produced no run to wait for.
#[derive(Debug)]
pub(crate) struct NotStarted {
    pub error: MetaWorkflowError,
    /// The door refused or could not be called, so it started nothing and
    /// nothing persisted for it is in use. False when it answered 200 without
    /// naming its run, or never answered: a run may exist that nobody watches.
    pub nothing_started: bool,
}

/// Start a run through `door` (the in-process run door's call) and return the
/// run it started, guarded.
///
/// The door call runs in a task of its own, which builds the guard the moment
/// the door names its run and hands it over through a channel. So an execute
/// dropped while the door call is in flight cannot leave a started run
/// unguarded: the send to a dropped receiver fails and the guard drops in that
/// task, or the guard waits in the channel and drops with it. Either way the
/// run is stopped.
pub(crate) async fn start_run<F>(
    ledger: Arc<dyn RunLedger>,
    door: F,
) -> Result<StopOnDrop, NotStarted>
where
    F: std::future::Future<Output = Result<DoorReply, String>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let started = match door.await {
            Err(e) => Err(NotStarted {
                error: MetaWorkflowError::Failed(e),
                nothing_started: true,
            }),
            Ok(reply) => match started_task_run(&reply) {
                Ok(task_run_id) => Ok(StopOnDrop::armed(ledger, task_run_id)),
                Err(error) => Err(NotStarted {
                    error,
                    nothing_started: reply.status != 200,
                }),
            },
        };
        // A send to an execute that has gone away hands the value back, and
        // the guard inside it drops here, stopping the run.
        let _ = tx.send(started);
    });
    rx.await.unwrap_or_else(|_| {
        Err(NotStarted {
            error: MetaWorkflowError::Failed(
                "the run door's task ended without answering".to_string(),
            ),
            nothing_started: false,
        })
    })
}

/// Run `meta_wf` on behalf of `origin` and return its output. `claim` is the
/// proposal claim the execute holds, when it holds one. See the module docs for
/// the four steps.
///
/// Listed in `runner_spawn_sites.txt` as an autonomous spawn site: it calls the
/// in-process spawn door, and it branches on `authorize_spawn` before anything
/// else.
pub(crate) async fn execute_meta_workflow_blocking(
    pg_db: Arc<PgDb>,
    meta_wf: crate::unified_workflows::UnifiedWorkflow,
    origin: SpawnOrigin,
    claim: Option<RunClaim>,
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

    let path = run_door_path(&saved.id);
    let started = start_run(pg_db.clone(), async move {
        crate::commands::operator_doors::call_door_in_process(&path, run_door_body(), origin).await
    })
    .await;
    let guard = match started {
        Ok(guard) => guard,
        Err(not_started) => {
            if not_started.nothing_started {
                // The door started nothing: take back the meta-workflow
                // persisted for it.
                if let Err(e) = pg_db.delete_unified_workflow(&saved.id).await {
                    warn!(
                        meta_workflow_id = %saved.id,
                        "spec execute: the run door started nothing; deleting its meta-workflow failed: {e}"
                    );
                }
            } else {
                warn!(
                    meta_workflow_id = %saved.id,
                    "spec execute: the run door did not name a run, so a run it may have \
                     started is unwatched: {}",
                    not_started.error
                );
            }
            return Err(not_started.error);
        }
    };
    info!(
        meta_workflow_id = %saved.id,
        task_run_id = %guard.task_run_id(),
        origin = %origin,
        "spec execute: meta-workflow run started"
    );

    await_run_output(
        pg_db,
        guard,
        claim.as_ref(),
        configured_execute_timeout(),
        POLL_INTERVAL,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::operator_doors::door_reply;
    use crate::coord_drain_state::api_refusal;
    use crate::database::pg::task_runs::FINISHED_TASK_RUN_STATUSES;
    use crate::mcp::types::{api_error, ApiResponse};
    use crate::mcp::unified_workflows::RunUnifiedWorkflowResponse;
    use axum::http::StatusCode;
    use axum::Json;
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
    fn admission_defers_every_drain_gate_class_with_its_code() {
        for class in DeferClass::ALL {
            assert_eq!(
                admission(&SpawnDecision::DeferredByDrain {
                    reason: "the drain gate deferred it".into(),
                    class,
                }),
                Err(MetaWorkflowError::Deferred {
                    code: class.code(),
                    reason: "the drain gate deferred it".into(),
                }),
                "{class:?}"
            );
        }
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

    /// The production entry, end to end, on its refusal arm. The test puts the
    /// process-global drain cache back to its boot state, `Unknown` with no
    /// dispatch role, which defers every autonomous spawn. It holds
    /// `ROLE_TEST_LOCK` while it does, because the tests that cache a drain
    /// or a role into that same global hold it too. The execute must answer
    /// `Deferred` before it touches the database: this `PgDb` fails every
    /// query, so an execute that persisted (or started) anything before asking
    /// would answer `Failed` instead.
    #[tokio::test]
    async fn an_execute_the_drain_defers_persists_and_starts_nothing() {
        let _role_lock = crate::coord_drain_state::ROLE_TEST_LOCK.lock().await;
        crate::coord_drain_state::reset_global_for_test();
        assert!(
            matches!(
                crate::coord_drain_state::current(),
                crate::coord_drain_state::CoordDrainState::Unknown { .. }
            ),
            "precondition: the reset leaves the boot Unknown"
        );
        assert_eq!(crate::coord_drain_state::current_dispatch_role(), None);
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
            None,
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
        let take_back = at("delete_unified_workflow(");
        let wait = at("await_run_output(");
        assert!(
            body.get(wait..)
                .is_some_and(|rest| rest.contains("claim.as_ref()")),
            "the run is bound to the execute's proposal claim"
        );
        assert!(
            gate < admit && admit < persist && persist < start && start < take_back,
            "order must be authorize_spawn -> admission -> persist -> start, and a door \
             that started nothing takes the persisted meta-workflow back \
             (found {gate}, {admit}, {persist}, {start}, {take_back})"
        );
    }

    // -----------------------------------------------------------------
    // the run door's reply. The path and the body are pinned where they are
    // parsed: `operator_doors` (the in-process router) and
    // `mcp::unified_workflows` (the request type).
    // -----------------------------------------------------------------

    type RunDoorResult =
        Result<Json<ApiResponse<RunUnifiedWorkflowResponse>>, (StatusCode, Json<ApiResponse<()>>)>;

    /// A reply exactly as the run door produces it: `run_unified_workflow`'s
    /// result, through `door_reply`.
    fn from_the_door(result: RunDoorResult) -> DoorReply {
        door_reply(result)
    }

    #[test]
    fn the_task_run_the_door_started_is_read_from_its_own_reply() {
        let ok = from_the_door(Ok(Json(ApiResponse::success(RunUnifiedWorkflowResponse {
            task_run_id: "unified-workflow-wf-1-7".to_string(),
        }))));
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
            let reply = DoorReply {
                status: 200,
                body: body.clone(),
            };
            assert!(
                matches!(started_task_run(&reply), Err(MetaWorkflowError::Failed(_))),
                "{body}"
            );
        }
    }

    #[test]
    fn the_doors_own_drain_refusal_is_a_deferral_for_every_class() {
        for class in DeferClass::ALL {
            let refused = from_the_door(Err((
                StatusCode::CONFLICT,
                Json(api_refusal("coord has drained this device", class)),
            )));
            assert_eq!(
                started_task_run(&refused),
                Err(MetaWorkflowError::Deferred {
                    code: class.code(),
                    reason: "coord has drained this device".into()
                }),
                "{class:?}"
            );
        }
    }

    #[test]
    fn any_other_refusal_is_a_failure_naming_the_status() {
        let mut busy = api_error("busy");
        busy.code = Some("something_else".to_string());
        assert_eq!(
            started_task_run(&from_the_door(Err((StatusCode::CONFLICT, Json(busy))))),
            Err(MetaWorkflowError::Failed(
                "the run door answered 409: busy".into()
            ))
        );
        let not_found = from_the_door(Err((
            StatusCode::NOT_FOUND,
            Json(api_error("Unified workflow not found: wf-1")),
        )));
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

    /// The poll stops waiting on exactly the statuses a stop leaves alone. A
    /// status in one list and not the other is either a finished run the
    /// deadline would try to stop, or a stop that overwrites what the poll is
    /// still waiting on.
    #[test]
    fn the_poll_and_the_stop_agree_on_what_finished_means() {
        for status in FINISHED_TASK_RUN_STATUSES {
            assert_ne!(run_state(status), RunState::Running, "{status}");
        }
        for status in ["complete", "completed", "failed", "stopped"] {
            assert!(FINISHED_TASK_RUN_STATUSES.contains(&status), "{status}");
        }
        for status in ["running", "paused", "cancelling", "pending", ""] {
            assert!(!FINISHED_TASK_RUN_STATUSES.contains(&status), "{status}");
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

    /// Answers `status`, `output`, `record_on_claim` and `disable_resume` from
    /// scripts (the last answer of each repeats once its script runs out),
    /// records every stop it is asked for with whether it stopped anything,
    /// and logs every call in order. A stop is conditional, as the SQL is: it
    /// stops the run only while the status script's current answer is a run
    /// still going.
    struct ScriptedLedger {
        statuses: Mutex<VecDeque<Result<Option<RunStatus>, String>>>,
        outputs: Mutex<VecDeque<Result<Option<String>, String>>>,
        claim_records: Mutex<VecDeque<Result<ClaimRecord, String>>>,
        resume_answers: Mutex<VecDeque<Result<(), String>>>,
        output_reads: Mutex<usize>,
        /// When set, the run finishes just before any stop reaches it: the
        /// stop changes nothing, and every later status read sees this.
        finishes_first: Mutex<Option<RunStatus>>,
        /// When set, every stop fails with it.
        stop_error: Option<String>,
        stops: Mutex<Vec<(String, String, bool)>>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl ScriptedLedger {
        fn new(statuses: Vec<Result<Option<RunStatus>, String>>) -> Self {
            Self {
                statuses: Mutex::new(statuses.into()),
                outputs: Mutex::new(VecDeque::from([Ok(Some(String::new()))])),
                claim_records: Mutex::new(VecDeque::from([Ok(ClaimRecord::Recorded)])),
                resume_answers: Mutex::new(VecDeque::from([Ok(())])),
                output_reads: Mutex::new(0),
                finishes_first: Mutex::new(None),
                stop_error: None,
                stops: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn outputs(mut self, outputs: Vec<Result<Option<String>, String>>) -> Self {
            self.outputs = Mutex::new(outputs.into());
            self
        }

        fn claim_records(mut self, answers: Vec<Result<ClaimRecord, String>>) -> Self {
            self.claim_records = Mutex::new(answers.into());
            self
        }

        fn resume_answers(mut self, answers: Vec<Result<(), String>>) -> Self {
            self.resume_answers = Mutex::new(answers.into());
            self
        }

        fn finishing_first(self, status: &str, error: Option<&str>) -> Self {
            *self.finishes_first.lock().unwrap() = Some(RunStatus {
                status: status.into(),
                error_message: error.map(str::to_string),
            });
            self
        }

        fn failing_stops(mut self, error: &str) -> Self {
            self.stop_error = Some(error.to_string());
            self
        }

        fn arc(self) -> Arc<Self> {
            Arc::new(self)
        }

        fn stops(&self) -> Vec<(String, String, bool)> {
            self.stops.lock().unwrap().clone()
        }

        fn output_reads(&self) -> usize {
            *self.output_reads.lock().unwrap()
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn called(&self, name: &'static str) -> usize {
            self.calls().iter().filter(|c| **c == name).count()
        }

        fn log(&self, name: &'static str) {
            self.calls.lock().unwrap().push(name);
        }
    }

    fn next<T: Clone>(script: &Mutex<VecDeque<T>>) -> T {
        let mut script = script.lock().unwrap();
        if script.len() > 1 {
            script.pop_front().unwrap()
        } else {
            script.front().cloned().expect("a scripted answer")
        }
    }

    #[async_trait::async_trait]
    impl RunLedger for ScriptedLedger {
        async fn status(&self, _task_run_id: &str) -> Result<Option<RunStatus>, String> {
            self.log("status");
            next(&self.statuses)
        }

        async fn output(&self, _task_run_id: &str) -> Result<Option<String>, String> {
            self.log("output");
            *self.output_reads.lock().unwrap() += 1;
            next(&self.outputs)
        }

        async fn stop_if_unfinished(
            &self,
            task_run_id: &str,
            reason: &str,
        ) -> Result<bool, String> {
            self.log("stop");
            if let Some(e) = &self.stop_error {
                return Err(e.clone());
            }
            let stopped = {
                let mut statuses = self.statuses.lock().unwrap();
                if let Some(finished) = self.finishes_first.lock().unwrap().take() {
                    *statuses = VecDeque::from([Ok(Some(finished))]);
                }
                match statuses.front() {
                    Some(Ok(Some(current))) => run_state(&current.status) == RunState::Running,
                    Some(Ok(None)) => false,
                    _ => true,
                }
            };
            self.stops
                .lock()
                .unwrap()
                .push((task_run_id.to_string(), reason.to_string(), stopped));
            Ok(stopped)
        }

        async fn record_on_claim(
            &self,
            _claim: &RunClaim,
            _task_run_id: &str,
        ) -> Result<ClaimRecord, String> {
            self.log("record");
            next(&self.claim_records)
        }

        async fn disable_resume(&self, _task_run_id: &str) -> Result<(), String> {
            self.log("resume-off");
            next(&self.resume_answers)
        }
    }

    fn status(status: &str, error: Option<&str>) -> Result<Option<RunStatus>, String> {
        Ok(Some(RunStatus {
            status: status.into(),
            error_message: error.map(str::to_string),
        }))
    }

    fn output(text: &str) -> Result<Option<String>, String> {
        Ok(Some(text.to_string()))
    }

    const TIMEOUT: Duration = Duration::from_secs(60);
    const EVERY: Duration = Duration::from_secs(5);

    /// `await_run_output` on `task_run_id`, guarded, with no proposal claim.
    fn wait(
        ledger: &Arc<ScriptedLedger>,
        task_run_id: &str,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, MetaWorkflowError>> {
        let ledger: Arc<dyn RunLedger> = ledger.clone();
        let guard = StopOnDrop::armed(ledger.clone(), task_run_id.to_string());
        async move { await_run_output(ledger, guard, None, TIMEOUT, EVERY).await }
    }

    /// `await_run_output` on `task_run_id`, guarded, for an execute holding
    /// proposal claim `prop-1`.
    fn wait_claimed(
        ledger: &Arc<ScriptedLedger>,
        task_run_id: &str,
    ) -> impl std::future::Future<Output = Result<serde_json::Value, MetaWorkflowError>> {
        let ledger: Arc<dyn RunLedger> = ledger.clone();
        let guard = StopOnDrop::armed(ledger.clone(), task_run_id.to_string());
        let claim = RunClaim {
            proposal_id: "prop-1".into(),
            token: "claim-1".into(),
        };
        async move { await_run_output(ledger, guard, Some(&claim), TIMEOUT, EVERY).await }
    }

    #[tokio::test(start_paused = true)]
    async fn a_completed_run_returns_its_output_read_once_and_is_not_stopped() {
        let ledger = ScriptedLedger::new(vec![
            status("running", None),
            status("running", None),
            status("complete", None),
        ])
        .outputs(vec![output(r#"{"id":"page"}"#)])
        .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({ "id": "page" })));
        assert_eq!(
            ledger.output_reads(),
            1,
            "the output is read once, when the run has finished, never per poll"
        );
        tokio::task::yield_now().await;
        assert!(ledger.stops().is_empty(), "a finished run is never stopped");
    }

    #[tokio::test(start_paused = true)]
    async fn a_dag_completed_run_counts_as_finished() {
        let ledger = ScriptedLedger::new(vec![status("completed", None)])
            .outputs(vec![output("plain text")])
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({ "output_log": "plain text" })));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_output_read_is_retried_never_reported_as_empty_output() {
        let ledger = ScriptedLedger::new(vec![status("complete", None)])
            .outputs(vec![
                Err("PG pool error: timed out".into()),
                output(r#"{"ok":true}"#),
            ])
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({ "ok": true })));
        assert_eq!(ledger.output_reads(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_output_still_unreadable_at_the_deadline_is_that_error() {
        let ledger = ScriptedLedger::new(vec![status("complete", None)])
            .outputs(vec![Err("PG pool error: timed out".into())])
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 finished at its deadline, but reading it failed: \
                 PG pool error: timed out"
                    .into()
            ))
        );
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-1".to_string(), DEADLINE_STOP_REASON.to_string(), false)],
            "the stop found the run finished and changed nothing"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_run_returns_its_error_message() {
        let ledger = ScriptedLedger::new(vec![status("failed", Some("builder step failed"))]).arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed("builder step failed".into()))
        );
        tokio::task::yield_now().await;
        assert!(ledger.stops().is_empty());
        assert_eq!(ledger.output_reads(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stopped_run_is_a_failure_naming_the_stop() {
        let ledger = ScriptedLedger::new(vec![status("stopped", Some("User stopped"))]).arc();
        let out = wait(&ledger, "tr-1").await;
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
        let ledger = ScriptedLedger::new(vec![status("running", None)]).arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 was still running after 60s and was stopped".into()
            ))
        );
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-1".to_string(), DEADLINE_STOP_REASON.to_string(), true)],
            "stopped exactly once, by the deadline, not again by the guard"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_that_completes_as_its_deadline_passes_returns_its_output() {
        let ledger = ScriptedLedger::new(vec![status("running", None)])
            .finishing_first("complete", None)
            .outputs(vec![output(r#"{"id":"late"}"#)])
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({ "id": "late" })));
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-1".to_string(), DEADLINE_STOP_REASON.to_string(), false)],
            "the stop found the run finished and changed nothing"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_that_fails_as_its_deadline_passes_returns_its_failure() {
        let ledger = ScriptedLedger::new(vec![status("running", None)])
            .finishing_first("failed", Some("builder step failed"))
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed("builder step failed".into()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_that_fails_at_the_deadline_says_so() {
        let ledger = ScriptedLedger::new(vec![status("running", None)])
            .failing_stops("PG pool error: timed out")
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 was still running after 60s; \
                 stopping it failed: PG pool error: timed out"
                    .into()
            ))
        );
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.called("stop"),
            2,
            "a stop that failed leaves the guard armed, and it asks again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_vanished_run_fails_with_nothing_to_stop() {
        let ledger = ScriptedLedger::new(vec![Ok(None)]).arc();
        let out = wait(&ledger, "tr-1").await;
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
            status("complete", None),
        ])
        .outputs(vec![output(r#"{"ok":true}"#)])
        .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({ "ok": true })));
    }

    #[tokio::test(start_paused = true)]
    async fn an_execute_dropped_mid_poll_stops_its_run() {
        let ledger = ScriptedLedger::new(vec![status("running", None)]).arc();
        let poll = wait(&ledger, "tr-9");
        // Let the poll read the run at least once, then abandon it the way an
        // HTTP client hanging up drops its handler future.
        let raced = tokio::time::timeout(Duration::from_secs(12), poll).await;
        assert!(raced.is_err(), "the poll was still waiting when dropped");
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-9".to_string(), ABANDONED_STOP_REASON.to_string(), true)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_execute_dropped_after_its_run_finished_asks_and_changes_nothing() {
        let ledger = ScriptedLedger::new(vec![status("running", None)])
            .finishing_first("complete", None)
            .arc();
        let poll = wait(&ledger, "tr-9");
        let raced = tokio::time::timeout(Duration::from_secs(12), poll).await;
        assert!(raced.is_err(), "the poll was still waiting when dropped");
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-9".to_string(), ABANDONED_STOP_REASON.to_string(), false)],
            "the guard's stop is conditional: a run that finished first keeps its status"
        );
    }

    // -----------------------------------------------------------------
    // binding the run to its execute
    // -----------------------------------------------------------------

    /// Before the poll first waits, the run is recorded on the execute's
    /// proposal claim and its `auto_continue` is turned off, each once.
    #[tokio::test(start_paused = true)]
    async fn the_run_is_bound_to_its_execute_before_the_poll_waits() {
        let ledger = ScriptedLedger::new(vec![status("complete", None)])
            .outputs(vec![output("{}")])
            .arc();
        let out = wait_claimed(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({})));
        assert_eq!(
            ledger.calls(),
            vec!["record", "resume-off", "status", "output"],
            "bound first, once, then polled"
        );
    }

    /// A bind write that fails is retried at the next poll, until it lands.
    #[tokio::test(start_paused = true)]
    async fn a_bind_that_fails_is_retried_until_it_lands() {
        let ledger = ScriptedLedger::new(vec![
            status("running", None),
            status("running", None),
            status("complete", None),
        ])
        .claim_records(vec![
            Err("PG pool error: timed out".into()),
            Ok(ClaimRecord::Recorded),
        ])
        .resume_answers(vec![
            Err("PG pool error: timed out".into()),
            Err("PG pool error: timed out".into()),
            Ok(()),
        ])
        .outputs(vec![output("{}")])
        .arc();
        let out = wait_claimed(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({})));
        assert_eq!(ledger.called("record"), 2);
        assert_eq!(ledger.called("resume-off"), 3);
    }

    /// With no proposal claim there is nothing to record the run on; its
    /// `auto_continue` is still turned off.
    #[tokio::test(start_paused = true)]
    async fn a_run_with_no_claim_is_still_made_non_resumable() {
        let ledger = ScriptedLedger::new(vec![status("complete", None)])
            .outputs(vec![output("{}")])
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(out, Ok(serde_json::json!({})));
        assert_eq!(ledger.called("record"), 0);
        assert_eq!(ledger.called("resume-off"), 1);
    }

    /// A claim lost before the run is recorded on it (a scan took the
    /// proposal back) stops the run and fails the execute at once: nothing
    /// would record its outcome.
    #[tokio::test(start_paused = true)]
    async fn a_lost_claim_stops_the_run_and_fails_the_execute() {
        let ledger = ScriptedLedger::new(vec![status("running", None)])
            .claim_records(vec![Ok(ClaimRecord::Lost)])
            .arc();
        let out = wait_claimed(&ledger, "tr-1").await;
        match out {
            Err(MetaWorkflowError::Failed(m)) => {
                assert!(m.contains("the claim on proposal prop-1 was lost"), "{m}")
            }
            other => panic!("a lost claim fails the execute; got {other:?}"),
        }
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-1".to_string(), CLAIM_LOST_STOP_REASON.to_string(), true)],
            "stopped once, for the lost claim, not again by the guard"
        );
        assert_eq!(
            ledger.called("status"),
            0,
            "a run nobody will record is not waited on"
        );
    }

    /// A completed run whose row is gone by the time its output is read is a
    /// run that disappeared, never an empty output.
    #[tokio::test(start_paused = true)]
    async fn a_run_gone_before_its_output_is_read_is_not_empty_output() {
        let ledger = ScriptedLedger::new(vec![status("complete", None)])
            .outputs(vec![Ok(None)])
            .arc();
        let out = wait(&ledger, "tr-1").await;
        assert_eq!(
            out,
            Err(MetaWorkflowError::Failed(
                "meta-workflow task run tr-1 disappeared".into()
            ))
        );
    }

    // -----------------------------------------------------------------
    // start_run — the door call's own task
    // -----------------------------------------------------------------

    fn named_run(task_run_id: &str) -> Result<DoorReply, String> {
        Ok(DoorReply {
            status: 200,
            body: serde_json::json!({ "success": true, "data": { "task_run_id": task_run_id } }),
        })
    }

    /// The run the door starts is handed over guarded: dropping the guard
    /// stops it.
    #[tokio::test(start_paused = true)]
    async fn a_started_run_is_handed_over_guarded() {
        let ledger = ScriptedLedger::new(vec![status("running", None)]).arc();
        let started = start_run(ledger.clone(), async { named_run("tr-1") }).await;
        let guard = match started {
            Ok(guard) => guard,
            Err(not_started) => panic!("the door named its run: {not_started:?}"),
        };
        assert_eq!(guard.task_run_id(), "tr-1");
        drop(guard);
        tokio::task::yield_now().await;
        assert_eq!(
            ledger.stops(),
            vec![("tr-1".to_string(), ABANDONED_STOP_REASON.to_string(), true)]
        );
    }

    /// An execute dropped while the door call is in flight cannot leave the
    /// run the door then starts unguarded: it is stopped.
    #[tokio::test(start_paused = true)]
    async fn a_run_started_after_its_execute_was_dropped_is_stopped() {
        let ledger = ScriptedLedger::new(vec![status("running", None)]).arc();
        let door = async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            named_run("tr-late")
        };
        let raced =
            tokio::time::timeout(Duration::from_secs(1), start_run(ledger.clone(), door)).await;
        assert!(
            raced.is_err(),
            "the execute went away while the door call was in flight"
        );
        assert!(
            ledger.stops().is_empty(),
            "nothing to stop before the door answers"
        );
        tokio::time::sleep(Duration::from_secs(20)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            ledger.stops(),
            vec![(
                "tr-late".to_string(),
                ABANDONED_STOP_REASON.to_string(),
                true
            )]
        );
    }

    /// The door says whether it started nothing: a refusal or an unreachable
    /// door started nothing, while a 200 that names no run may have started
    /// one.
    #[tokio::test(start_paused = true)]
    async fn the_door_says_whether_it_started_nothing() {
        let ledger: Arc<dyn RunLedger> = ScriptedLedger::new(vec![status("running", None)]).arc();
        let refused = start_run(ledger.clone(), async {
            Ok(from_the_door(Err((
                StatusCode::NOT_FOUND,
                Json(api_error("Unified workflow not found: wf-1")),
            ))))
        })
        .await
        .err()
        .expect("a refusal starts no run");
        assert!(refused.nothing_started);
        let drained = start_run(ledger.clone(), async {
            Ok(from_the_door(Err((
                StatusCode::CONFLICT,
                Json(api_refusal(
                    "coord has drained this device",
                    DeferClass::Drained,
                )),
            ))))
        })
        .await
        .err()
        .expect("a drain refusal starts no run");
        assert!(drained.nothing_started);
        assert!(matches!(drained.error, MetaWorkflowError::Deferred { .. }));
        let unreachable = start_run(ledger.clone(), async {
            Err("no Tauri app handle — cannot call the door in-process".to_string())
        })
        .await
        .err()
        .expect("an unreachable door starts no run");
        assert!(unreachable.nothing_started);
        let unnamed = start_run(ledger.clone(), async {
            Ok(DoorReply {
                status: 200,
                body: serde_json::json!({ "success": true }),
            })
        })
        .await
        .err()
        .expect("a 200 with no run is not a run to wait on");
        assert!(
            !unnamed.nothing_started,
            "a 200 that names no run may have started one"
        );
    }
}
