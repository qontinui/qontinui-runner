//! `require_probe` — the scheduler condition that runs an external command.
//!
//! A task whose [`ScheduleConditions::require_probe`] is enabled may run only
//! while its probe command exits 0. Plan
//! `2026-09-29-quiet-is-measured-by-session-existence-and-machine-wide-so-a-24x7-box-never-gets-one`
//! Phase 4b: it lets a task wait for a condition only an external program can
//! decide (the return-to-main sweep's per-repo quiet check), instead of firing
//! at a fixed clock time and finding the box busy.
//!
//! # Semantics
//!
//! - **Met iff exit 0 inside the budget.** A non-zero exit, a timeout (the
//!   probe's whole process tree is killed at `timeout_seconds`) and a spawn
//!   failure (including an empty `command`) are all NOT met, and each is logged
//!   with its own message plus a bounded stderr tail. An outcome this module
//!   could not observe never reads as met.
//! - **Argv exec, never a shell.** `command[0]` is the program. A caller that
//!   wants shell syntax writes `["sh", "-c", "..."]` and owns the quoting.
//! - **Rate-limited to one run per `poll_seconds` per task** (floored at
//!   [`PROBE_POLL_FLOOR_SECS`], the scheduler's tick). Between polls the last
//!   result is reused — a NOT-met result deliberately persists until the next
//!   poll, which is the whole point of the rate limit.
//! - **A MET result is single-use and short-lived.** It is consumed by the run
//!   it admits ([`ProbeGate::consume`]), so a later evaluation — the next cron
//!   slot, or a `Condition` task's next rearm — always needs a fresh exit 0.
//!   And any result older than one poll interval has EXPIRED and reads as not
//!   met ("awaiting the next probe"), so a stale green can never admit a run.
//! - **The tick never waits long.** A probe runs on its own tokio task; the
//!   tick that launches it waits at most [`INLINE_GRACE`] for the answer (which
//!   covers any quick probe, so its answer is used on the same tick), and a
//!   slower probe's result is harvested by a later tick. One slow probe
//!   therefore cannot stall every other task's schedule.
//! - **A changed probe config resets the task's state**, aborting (and so
//!   killing) any probe still running under the old command.
//!
//! State is in memory, keyed by task id. A runner restart forgets it, which
//! only costs one extra probe run.
//!
//! [`ScheduleConditions::require_probe`]: crate::scheduler::ScheduleConditions

use crate::scheduler::{ProbeCondition, ScheduleConditions};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Lowest honoured `poll_seconds`: the scheduler evaluates conditions once per
/// 60 s tick, so a shorter interval could not be delivered.
pub const PROBE_POLL_FLOOR_SECS: u32 = 60;

/// Highest accepted `timeout_seconds` (one hour). A probe is a check, not a
/// job; anything slower belongs in the task itself.
pub const PROBE_TIMEOUT_MAX_SECS: u32 = 3600;

/// How long the tick that launches a probe waits inline for its result before
/// moving on and leaving it to be harvested by a later tick.
pub const INLINE_GRACE: Duration = Duration::from_secs(10);

/// How many trailing bytes of the probe's stderr are kept for the log line.
const STDERR_TAIL_BYTES: usize = 1024;

/// After the child exits, how long to keep reading stderr for EOF. Bounded
/// because a descendant that outlived the child may hold the write end open
/// forever.
const STDERR_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Refuse a `require_probe` the scheduler could not honour as written.
///
/// Called by the create/update task handlers so a bad value is a 400 the
/// author sees, never a silently rewritten one. `enabled: false` is still
/// validated: flipping it on later must not arm a probe that cannot run.
pub fn validate_conditions(conditions: &ScheduleConditions) -> Result<(), String> {
    let Some(probe) = &conditions.require_probe else {
        return Ok(());
    };
    if probe.command.is_empty() || probe.command[0].trim().is_empty() {
        return Err("requireProbe.command must name a program (argv[0] is empty)".to_string());
    }
    if probe.poll_seconds < PROBE_POLL_FLOOR_SECS {
        return Err(format!(
            "requireProbe.pollSeconds must be at least {PROBE_POLL_FLOOR_SECS} (the scheduler \
             ticks once a minute), got {}",
            probe.poll_seconds
        ));
    }
    if probe.timeout_seconds == 0 || probe.timeout_seconds > PROBE_TIMEOUT_MAX_SECS {
        return Err(format!(
            "requireProbe.timeoutSeconds must be 1..={PROBE_TIMEOUT_MAX_SECS}, got {}",
            probe.timeout_seconds
        ));
    }
    Ok(())
}

/// What one probe run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Exited 0 inside the budget — the condition is met.
    Met,
    /// Exited on its own with a non-zero status. `code` is `None` when the
    /// process was ended by a signal.
    Exited {
        code: Option<i32>,
        stderr_tail: String,
    },
    /// Still running at `timeout_seconds`; its process tree was killed.
    TimedOut { after: Duration, stderr_tail: String },
    /// Never ran: empty command, missing program, permission denied, or the
    /// probe task itself died.
    SpawnFailed { error: String },
}

impl ProbeOutcome {
    pub fn is_met(&self) -> bool {
        matches!(self, ProbeOutcome::Met)
    }

    /// One-line human description, stored in `ConditionStatus.probeDetail`.
    pub fn detail(&self) -> String {
        fn with_tail(head: String, tail: &str) -> String {
            if tail.is_empty() {
                head
            } else {
                format!("{head}; stderr: {tail}")
            }
        }
        match self {
            ProbeOutcome::Met => "exit 0".to_string(),
            ProbeOutcome::Exited {
                code: Some(code),
                stderr_tail,
            } => with_tail(format!("exit {code}"), stderr_tail),
            ProbeOutcome::Exited {
                code: None,
                stderr_tail,
            } => with_tail("terminated by a signal".to_string(), stderr_tail),
            ProbeOutcome::TimedOut { after, stderr_tail } => {
                with_tail(format!("timed out after {}s", after.as_secs()), stderr_tail)
            }
            ProbeOutcome::SpawnFailed { error } => format!("spawn failed: {error}"),
        }
    }

    /// Log this outcome — each failure class with its own message, so a
    /// timeout is never mistaken for an ordinary "not yet".
    fn log(&self, task_id: &str, command: &[String]) {
        match self {
            ProbeOutcome::Met => {
                info!(task_id, ?command, "scheduler probe: exit 0 — condition met");
            }
            ProbeOutcome::Exited { code, stderr_tail } => {
                info!(
                    task_id,
                    ?command,
                    ?code,
                    stderr_tail = %stderr_tail,
                    "scheduler probe: non-zero exit — condition NOT met"
                );
            }
            ProbeOutcome::TimedOut { after, stderr_tail } => {
                warn!(
                    task_id,
                    ?command,
                    after_secs = after.as_secs(),
                    stderr_tail = %stderr_tail,
                    "scheduler probe: TIMED OUT and was killed — condition NOT met"
                );
            }
            ProbeOutcome::SpawnFailed { error } => {
                warn!(
                    task_id,
                    ?command,
                    error = %error,
                    "scheduler probe: could not be spawned — condition NOT met"
                );
            }
        }
    }
}

/// Keeps the last [`STDERR_TAIL_BYTES`] of a stream, draining (and
/// discarding) the rest so the child never blocks on a full pipe.
async fn read_tail<R: tokio::io::AsyncRead + Unpin>(mut reader: R, sink: Arc<StdMutex<Vec<u8>>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut tail = sink.lock().unwrap_or_else(|p| p.into_inner());
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > STDERR_TAIL_BYTES {
                    let excess = tail.len() - STDERR_TAIL_BYTES;
                    tail.drain(..excess);
                }
            }
        }
    }
}

fn tail_string(sink: &StdMutex<Vec<u8>>) -> String {
    let tail = sink.lock().unwrap_or_else(|p| p.into_inner());
    String::from_utf8_lossy(&tail).trim().to_string()
}

/// Run `command` once: argv exec (no shell), stdin/stdout discarded, stderr
/// tail kept, whole process tree killed at `timeout`.
///
/// The child is its own process-group leader
/// ([`ChildTreeGuard`](crate::process_helpers::ChildTreeGuard)), so a timeout —
/// or the probe's task being aborted, which drops this future — kills every
/// descendant too, not only the direct child (`kill_on_drop` alone would miss a
/// grandchild, and a quiet-check script is exactly a tree of grandchildren). On
/// a normal exit the group is released, not killed, matching
/// `process_helpers::run_with_timeout_detailed`.
pub async fn run_probe(command: &[String], timeout: Duration) -> ProbeOutcome {
    use crate::process_helpers::ChildTreeGuard;
    use std::process::Stdio;

    let Some((program, args)) = command.split_first() else {
        return ProbeOutcome::SpawnFailed {
            error: "empty command".to_string(),
        };
    };
    if program.trim().is_empty() {
        return ProbeOutcome::SpawnFailed {
            error: "empty program name".to_string(),
        };
    }

    let mut cmd = crate::process_helpers::tokio_no_window(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    ChildTreeGuard::arm_tokio(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return ProbeOutcome::SpawnFailed {
                error: format!("{program}: {e}"),
            }
        }
    };
    let tree = ChildTreeGuard::attach_armed_tokio(&child);

    let sink = Arc::new(StdMutex::new(Vec::new()));
    let reader = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(read_tail(stderr, sink.clone())));

    // `None` = still running at the deadline.
    let waited = tokio::time::timeout(timeout, child.wait()).await.ok();

    match waited {
        None => {
            // Fire the tree guard (SIGKILL the group), then the direct child.
            drop(tree);
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            if let Some(reader) = reader {
                let _ = tokio::time::timeout(Duration::from_millis(500), reader).await;
            }
            ProbeOutcome::TimedOut {
                after: timeout,
                stderr_tail: tail_string(&sink),
            }
        }
        Some(Err(e)) => {
            drop(tree);
            ProbeOutcome::SpawnFailed {
                error: format!("{program}: waiting for the probe failed: {e}"),
            }
        }
        Some(Ok(status)) => {
            tree.disarm();
            if let Some(mut reader) = reader {
                // A descendant may still hold stderr open; do not wait for it.
                if tokio::time::timeout(STDERR_DRAIN_GRACE, &mut reader)
                    .await
                    .is_err()
                {
                    reader.abort();
                }
            }
            if status.success() {
                ProbeOutcome::Met
            } else {
                ProbeOutcome::Exited {
                    code: status.code(),
                    stderr_tail: tail_string(&sink),
                }
            }
        }
    }
}

/// The per-task in-memory probe state.
struct ProbeState {
    /// The config the state below was produced under. A different config is a
    /// different probe, so the state is reset.
    config: ProbeCondition,
    /// When the most recent probe run was launched (the rate-limit clock).
    last_started: Option<Instant>,
    /// A launched probe whose result has not been harvested yet.
    in_flight: Option<JoinHandle<ProbeOutcome>>,
    /// The most recent harvested result and when it was harvested. `None`
    /// after [`ProbeGate::consume`].
    last: Option<(ProbeOutcome, Instant)>,
}

impl ProbeState {
    fn new(config: ProbeCondition) -> Self {
        Self {
            config,
            last_started: None,
            in_flight: None,
            last: None,
        }
    }

    fn reset(&mut self, config: ProbeCondition) {
        if let Some(handle) = self.in_flight.take() {
            handle.abort();
        }
        *self = Self::new(config);
    }
}

impl Drop for ProbeState {
    fn drop(&mut self) {
        if let Some(handle) = self.in_flight.take() {
            handle.abort();
        }
    }
}

/// The verdict [`ProbeGate::evaluate`] returns for one evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeVerdict {
    pub met: bool,
    pub detail: String,
}

/// Rate-limited, in-memory `require_probe` evaluation for every task.
#[derive(Default)]
pub struct ProbeGate {
    states: Mutex<HashMap<String, ProbeState>>,
}

fn effective_poll(config: &ProbeCondition) -> Duration {
    Duration::from_secs(u64::from(config.poll_seconds.max(PROBE_POLL_FLOOR_SECS)))
}

fn effective_timeout(config: &ProbeCondition) -> Duration {
    Duration::from_secs(u64::from(
        config.timeout_seconds.clamp(1, PROBE_TIMEOUT_MAX_SECS),
    ))
}

impl ProbeGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate `config` for `task_id` at `now` (the caller's clock, so the
    /// rate limit can be driven deterministically in tests).
    ///
    /// Launches a probe when none is running and one poll interval has passed
    /// since the last launch, waiting up to [`INLINE_GRACE`] for it. The
    /// verdict is the latest unexpired, unconsumed result; with none, it is
    /// NOT met.
    pub async fn evaluate(&self, task_id: &str, config: &ProbeCondition, now: Instant) -> ProbeVerdict {
        let mut states = self.states.lock().await;
        let state = states
            .entry(task_id.to_string())
            .or_insert_with(|| ProbeState::new(config.clone()));
        if state.config != *config {
            info!(task_id, "scheduler probe: config changed — resetting probe state");
            state.reset(config.clone());
        }

        // Harvest a probe launched on an earlier tick that has since finished.
        if state.in_flight.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(handle) = state.in_flight.take() {
                let outcome = join_outcome(handle.await);
                outcome.log(task_id, &config.command);
                state.last = Some((outcome, now));
            }
        }

        let poll = effective_poll(config);
        let due = state.in_flight.is_none()
            && state
                .last_started
                .is_none_or(|started| now.saturating_duration_since(started) >= poll);
        if due {
            let command = config.command.clone();
            let timeout = effective_timeout(config);
            let mut handle = tokio::spawn(async move { run_probe(&command, timeout).await });
            state.last_started = Some(now);
            match tokio::time::timeout(INLINE_GRACE, &mut handle).await {
                Ok(joined) => {
                    let outcome = join_outcome(joined);
                    outcome.log(task_id, &config.command);
                    state.last = Some((outcome, now));
                }
                Err(_) => state.in_flight = Some(handle),
            }
        }

        match &state.last {
            Some((outcome, at)) if now.saturating_duration_since(*at) <= poll => ProbeVerdict {
                met: outcome.is_met(),
                detail: outcome.detail(),
            },
            Some(_) => ProbeVerdict {
                met: false,
                detail: "last result expired; awaiting the next probe".to_string(),
            },
            None if state.in_flight.is_some() => ProbeVerdict {
                met: false,
                detail: "probe running; awaiting its result".to_string(),
            },
            None => ProbeVerdict {
                met: false,
                detail: "awaiting the next probe".to_string(),
            },
        }
    }

    /// Spend the task's MET result on the run it just admitted, so it cannot
    /// admit a second one. The rate-limit clock is kept.
    pub async fn consume(&self, task_id: &str) {
        if let Some(state) = self.states.lock().await.get_mut(task_id) {
            state.last = None;
        }
    }

    /// Drop all state for a task (condition wait timed out, or task gone),
    /// aborting — and so killing — any probe still running for it.
    pub async fn forget(&self, task_id: &str) {
        self.states.lock().await.remove(task_id);
    }

    /// Drop state for every task not in `live_task_ids`.
    pub async fn retain(&self, live_task_ids: &[&str]) {
        self.states
            .lock()
            .await
            .retain(|id, _| live_task_ids.contains(&id.as_str()));
    }
}

fn join_outcome(joined: Result<ProbeOutcome, tokio::task::JoinError>) -> ProbeOutcome {
    joined.unwrap_or_else(|e| ProbeOutcome::SpawnFailed {
        error: format!("probe task did not complete: {e}"),
    })
}

/// Process-running tests use `sh`, so they are Unix-only; the validation and
/// clamping tests at the bottom run everywhere.
#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    fn probe(command: Vec<String>) -> ProbeCondition {
        ProbeCondition {
            enabled: true,
            command,
            poll_seconds: 60,
            timeout_seconds: 5,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_zero_is_met() {
        assert_eq!(run_probe(&sh("exit 0"), Duration::from_secs(5)).await, ProbeOutcome::Met);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_zero_exit_is_not_met_and_keeps_a_stderr_tail() {
        let outcome = run_probe(&sh("echo busy-repo >&2; exit 3"), Duration::from_secs(5)).await;
        assert_eq!(
            outcome,
            ProbeOutcome::Exited {
                code: Some(3),
                stderr_tail: "busy-repo".to_string()
            }
        );
        assert!(!outcome.is_met());
        assert_eq!(outcome.detail(), "exit 3; stderr: busy-repo");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_stderr_tail_is_bounded() {
        let outcome = run_probe(
            &sh("head -c 100000 /dev/zero | tr '\\0' x >&2; echo END >&2; exit 1"),
            Duration::from_secs(10),
        )
        .await;
        let ProbeOutcome::Exited { stderr_tail, .. } = outcome else {
            panic!("expected Exited, got {outcome:?}");
        };
        assert!(stderr_tail.len() <= STDERR_TAIL_BYTES, "{}", stderr_tail.len());
        assert!(stderr_tail.ends_with("END"), "the tail is the END of the stream");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_is_not_met_and_kills_the_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild-survived");
        // The grandchild would write the marker after 3 s; the probe times out
        // at 1 s, and the group kill must take the grandchild down with it.
        let script = format!(
            "(sleep 3; touch '{}') & echo waiting >&2; sleep 30",
            marker.display()
        );
        let started = Instant::now();
        let outcome = run_probe(&sh(&script), Duration::from_secs(1)).await;
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert_eq!(
            outcome,
            ProbeOutcome::TimedOut {
                after: Duration::from_secs(1),
                stderr_tail: "waiting".to_string()
            }
        );
        assert!(!outcome.is_met());
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "the timed-out probe's grandchild must be killed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn spawn_failure_is_not_met() {
        let outcome = run_probe(
            &["/nonexistent/qontinui-probe-binary".to_string()],
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(outcome, ProbeOutcome::SpawnFailed { .. }), "{outcome:?}");
        assert!(!outcome.is_met());
        assert!(outcome.detail().starts_with("spawn failed: /nonexistent/qontinui-probe-binary"));

        let empty = run_probe(&[], Duration::from_secs(5)).await;
        assert_eq!(
            empty,
            ProbeOutcome::SpawnFailed {
                error: "empty command".to_string()
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn argv_is_exec_not_shell() {
        // A shell would expand `$HOME` and treat `;` as a separator; argv exec
        // hands `test` the literal strings, and `test` with 3 args compares them.
        let outcome = run_probe(
            &[
                "test".into(),
                "$HOME;exit".into(),
                "=".into(),
                "$HOME;exit".into(),
            ],
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(outcome, ProbeOutcome::Met);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gate_rate_limits_and_not_met_persists_between_polls() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("runs");
        let script = format!("echo x >> '{}'; exit 1", counter.display());
        let config = probe(sh(&script));
        let gate = ProbeGate::new();
        let t0 = Instant::now();
        let runs = || {
            std::fs::read_to_string(&counter)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        };

        let v = gate.evaluate("t", &config, t0).await;
        assert!(!v.met);
        assert_eq!(v.detail, "exit 1");
        assert_eq!(runs(), 1);

        // Inside the poll interval: no new run, the NOT-met result is reused.
        let v = gate.evaluate("t", &config, t0 + Duration::from_secs(30)).await;
        assert!(!v.met);
        assert_eq!(v.detail, "exit 1");
        assert_eq!(runs(), 1);

        // One poll later: a new run.
        gate.evaluate("t", &config, t0 + Duration::from_secs(60)).await;
        assert_eq!(runs(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_met_result_is_consumed_by_the_run_it_admits() {
        let config = probe(sh("exit 0"));
        let gate = ProbeGate::new();
        let t0 = Instant::now();
        assert!(gate.evaluate("t", &config, t0).await.met);
        gate.consume("t").await;
        // Same poll window: no new probe may run, and the spent result is gone.
        let v = gate.evaluate("t", &config, t0 + Duration::from_secs(10)).await;
        assert!(!v.met, "a consumed MET must not admit a second run");
        // Next poll: fresh probe, met again.
        assert!(gate.evaluate("t", &config, t0 + Duration::from_secs(60)).await.met);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_result_older_than_one_poll_expires_to_not_met() {
        // The expiry arm is reached when a probe is still in flight (so none
        // is relaunched) past one poll after the last result was harvested.
        let config = ProbeCondition {
            enabled: true,
            command: sh("exit 0"),
            poll_seconds: 60,
            timeout_seconds: 5,
        };
        let gate = ProbeGate::new();
        let t0 = Instant::now();
        assert!(gate.evaluate("t", &config, t0).await.met);
        // Force the in-flight arm: install a never-finishing handle so the
        // next evaluation cannot relaunch, then jump past the poll.
        {
            let mut states = gate.states.lock().await;
            let state = states.get_mut("t").unwrap();
            state.in_flight = Some(tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                ProbeOutcome::Met
            }));
        }
        let v = gate.evaluate("t", &config, t0 + Duration::from_secs(61)).await;
        assert!(!v.met, "a MET older than one poll must not admit a run");
        assert_eq!(v.detail, "last result expired; awaiting the next probe");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_changed_config_resets_the_rate_limit() {
        let gate = ProbeGate::new();
        let t0 = Instant::now();
        assert!(!gate.evaluate("t", &probe(sh("exit 1")), t0).await.met);
        // Same instant, different command: a new probe runs immediately.
        assert!(gate.evaluate("t", &probe(sh("exit 0")), t0).await.met);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_slow_probe_is_harvested_by_a_later_evaluation() {
        let mut config = probe(sh("sleep 12; exit 0"));
        config.timeout_seconds = 30;
        let gate = ProbeGate::new();
        let t0 = Instant::now();
        let v = gate.evaluate("t", &config, t0).await;
        assert!(!v.met);
        assert_eq!(v.detail, "probe running; awaiting its result");
        tokio::time::sleep(Duration::from_secs(4)).await;
        let v = gate.evaluate("t", &config, t0 + Duration::from_secs(20)).await;
        assert!(v.met, "{v:?}");
    }

    #[test]
    fn validation_refuses_what_the_scheduler_cannot_honour() {
        let ok = ScheduleConditions {
            require_probe: Some(probe(sh("exit 0"))),
            ..Default::default()
        };
        assert!(validate_conditions(&ok).is_ok());
        assert!(validate_conditions(&ScheduleConditions::default()).is_ok());

        let mut empty = ok.clone();
        empty.require_probe.as_mut().unwrap().command.clear();
        assert!(validate_conditions(&empty).unwrap_err().contains("command"));

        let mut fast = ok.clone();
        fast.require_probe.as_mut().unwrap().poll_seconds = 59;
        assert!(validate_conditions(&fast).unwrap_err().contains("pollSeconds"));

        let mut zero = ok.clone();
        zero.require_probe.as_mut().unwrap().timeout_seconds = 0;
        assert!(validate_conditions(&zero).unwrap_err().contains("timeoutSeconds"));

        let mut long = ok;
        long.require_probe.as_mut().unwrap().timeout_seconds = PROBE_TIMEOUT_MAX_SECS + 1;
        assert!(validate_conditions(&long).unwrap_err().contains("timeoutSeconds"));
    }

    #[test]
    fn a_stored_sub_floor_poll_is_clamped_at_evaluation() {
        let mut config = probe(sh("exit 0"));
        config.poll_seconds = 5;
        config.timeout_seconds = 0;
        assert_eq!(effective_poll(&config), Duration::from_secs(60));
        assert_eq!(effective_timeout(&config), Duration::from_secs(1));
    }
}
