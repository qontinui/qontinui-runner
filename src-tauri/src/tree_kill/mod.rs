//! Kill a process tree WITHOUT creating a process, on every OS the runner
//! ships on — and kill a TRACKED AI pid only while it is provably the process
//! that was tracked.
//!
//! Plan `2026-10-19-served-corpus-probe-budget-excludes-process-creation`,
//! Phase 4, replaced every Windows `taskkill /F /T /PID` spawn with the
//! in-process Toolhelp walk in [`qontinui_runner_win32::process_tree`]: the
//! root and every descendant linked to it through a parent this walk holds a
//! handle on, with a pid-reuse guard (a process created before its recorded
//! parent is never admitted).
//!
//! Plan `2026-10-25-runner-kill-paths-never-kill-anything-off-windows`,
//! Phase 2, gives the same helpers a Unix arm ([`unix`]): on Linux the freeze
//! walk in `qontinui_pty_holder::tree_freeze` (the root must be an unreaped
//! child of this runner; every member is stopped before it is admitted, so no
//! stranger is ever signalled), and on other Unixes the root-only rule of the
//! plan's D-1 (compile-unverified: CI builds no macOS leg).
//!
//! **Tracked AI pids (D-2).** The runner-global AI pid tracker holds bare
//! pids, and a bare pid is a name that is recycled once its process is reaped.
//! Its waiters reap the child before the pid leaves the tracker, and the
//! runner has many other direct children, so "this pid is an unreaped child of
//! the runner" is not proof that it is the tracked process. Every tracker
//! write therefore goes through [`track_ai_pid`], which records the pid's
//! START TOKEN (`qontinui_pty_holder::proc_identity::start_token`) while the
//! `Child` is still unwaited and the pid certainly names it. A tracked pid is
//! killed only when its current identity still matches that token — on Linux
//! inside the freeze walk's pidfd window, on Windows through a handle opened
//! BEFORE the creation time is compared — and a pid with no recorded identity
//! is refused, never signalled. A process the Doctor lists but no tracker
//! holds gets the same record through [`record_spawn_identity`], so the
//! Doctor's force-stop reaches it under the same proof.
//!
//! **Counts count kills (D-4).** [`KillTally`] counts a pid as killed only
//! when its tree kill returned `Ok` with a non-empty `killed`. A root that had
//! already exited is `already_exited`; every `Err` is `refused`, with its
//! reason.
//!
//! [`kill_tree_by_pid`] takes a bare pid and is only as good as the moment
//! the pid was read. [`kill_child_tree`] roots the walk at a `Child`, which
//! pins the identity for as long as the `Child` is alive and unwaited. Both
//! refuse the runner's own pid.

#[cfg(unix)]
mod unix;

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use tracing::{info, warn};

pub(crate) use qontinui_runner_win32::process_tree::TreeKillReport;

/// Whether this OS has a start token at all. Where it has none (macOS), a
/// tracked pid's kill rests on the "unreaped child of this runner" proof
/// alone; where it has one, a pid whose token was unreadable when it was
/// tracked is refused.
const START_TOKENS_EXIST: bool = cfg!(any(target_os = "linux", windows));

/// End `pid` and its descendants. `Err` names the pid when it cannot be
/// reached (it already exited, it is not ours — on Unix, not an unreaped
/// child of this runner), when it is the runner itself, or when it survives
/// the kill.
///
/// **Unix precondition:** the caller must hold this pid's `std` or `tokio`
/// `Child` UNWAITED for the duration of the call. The Linux walk proves only
/// that the pid is SOME unreaped child of the runner; an unwaited `Child` is
/// what makes it THIS child. A tracked AI pid, whose `Child` lives elsewhere,
/// goes through [`kill_tracked_pid`] instead, which checks its recorded start
/// token.
pub(crate) fn kill_tree_by_pid(pid: u32, budget: Duration) -> Result<TreeKillReport, String> {
    #[cfg(windows)]
    {
        qontinui_runner_win32::process_tree::terminate_process_tree(pid, budget)
            .map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    {
        unix::kill_tree(pid, None, budget)
    }
}

/// [`kill_tree_by_pid`] off the async runtime: the walk is short, but it is
/// blocking OS work, and a runtime worker must not do it. Same Unix
/// precondition: the caller holds the pid's `Child` unwaited.
pub(crate) async fn kill_tree_by_pid_async(
    pid: u32,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    tokio::task::spawn_blocking(move || kill_tree_by_pid(pid, budget))
        .await
        .map_err(|e| format!("pid {pid}: tree-kill task failed: {e}"))?
}

/// End `child` and its descendants, rooted at the child itself. The `Child`
/// must not have been waited: an unwaited child's pid cannot be reused.
pub(crate) fn kill_child_tree(
    child: &std::process::Child,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        let root = qontinui_runner_win32::process_tree::OwnedProcess::duplicate(child.as_handle())
            .map_err(|e| format!("pid {}: cannot duplicate the child handle: {e}", child.id()))?;
        qontinui_runner_win32::process_tree::terminate_tree_of(&root, child.id(), budget)
            .map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    {
        unix::kill_tree(child.id(), None, budget)
    }
}

// ---------------------------------------------------------------------------
// Tracked AI pids (D-2)
// ---------------------------------------------------------------------------

/// What was recorded for one pid: by [`track_ai_pid`] (a tracker entry), or
/// by [`record_spawn_identity`] (a Doctor-listed process in no tracker).
#[derive(Debug, Clone)]
struct Recorded {
    /// The pid's start token at record time; `None` when unreadable, or on an
    /// OS with no token.
    token: Option<String>,
    /// The tracker the pid was pushed into (its address), so pruning one
    /// tracker's departed pids never touches another tracker's entries.
    /// `None` for a [`record_spawn_identity`] record.
    tracker: Option<usize>,
}

/// Spawn identities of tracked and Doctor-listed pids, keyed by pid.
static SPAWN_IDENTITY: LazyLock<Mutex<HashMap<u32, Recorded>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn identities() -> MutexGuard<'static, HashMap<u32, Recorded>> {
    SPAWN_IDENTITY.lock().unwrap_or_else(|p| p.into_inner())
}

/// Poison-recovering: a panicked writer elsewhere must not turn a kill into a
/// no-op.
fn lock_tracker(tracker: &Mutex<Vec<u32>>) -> MutexGuard<'_, Vec<u32>> {
    tracker.lock().unwrap_or_else(|p| p.into_inner())
}

fn tracker_key(tracker: &Mutex<Vec<u32>>) -> usize {
    tracker as *const Mutex<Vec<u32>> as usize
}

/// The start token of `pid` now. The caller must hold the pid's `Child`
/// UNWAITED, so that the pid certainly names it: that is what makes the token
/// the child's spawn identity.
pub(crate) fn spawn_token(pid: u32) -> Option<String> {
    qontinui_pty_holder::proc_identity::start_token(pid)
}

/// THE writer of an AI pid tracker. Push `pid` — a child this runner has just
/// spawned and not yet waited, so the pid certainly names it — and record its
/// start token, against which every later kill of the pid is checked. Returns
/// that token, for a per-task registration of the same pid (see
/// [`TaskPids`]).
///
/// Entries of this tracker whose pid has since left it (its waiter removed
/// it) are pruned here, so the record stays as small as the tracker.
pub(crate) fn track_ai_pid(tracker: &Mutex<Vec<u32>>, pid: u32) -> Option<String> {
    let token = spawn_token(pid);
    if token.is_none() && START_TOKENS_EXIST {
        warn!(
            target: "tree_kill",
            "tracked AI pid {pid}: its start token is unreadable, so a stop will refuse to \
             signal it"
        );
    }
    let key = tracker_key(tracker);
    let mut pids = lock_tracker(tracker);
    pids.push(pid);
    let mut ids = identities();
    ids.retain(|p, r| r.tracker != Some(key) || pids.contains(p));
    ids.insert(
        pid,
        Recorded {
            token: token.clone(),
            tracker: Some(key),
        },
    );
    token
}

/// Record the spawn identity of a process the Doctor lists but no AI pid
/// tracker holds (a one-shot whose `Child` is being waited elsewhere, an
/// interactive session spawned without a tracker), so the Doctor's force-stop
/// can kill it while it is provably that process. The caller must hold the
/// `Child` UNWAITED. Returns the token, which [`forget_spawn_identity`] takes.
pub(crate) fn record_spawn_identity(pid: u32) -> Option<String> {
    let token = spawn_token(pid);
    record_spawn_token(pid, token.clone());
    token
}

/// [`record_spawn_identity`] with a token the caller read earlier, while the
/// `Child` was still unwaited (a session whose waiter thread may since have
/// reaped it).
///
/// Such a record needs a token on every OS (its `Child` is waited on another
/// thread, so "an unreaped child of this runner" proves nothing), so an
/// unreadable one records nothing and the force-stop refuses. A pid that is
/// already tracked under the same identity keeps its tracker record, and a
/// tracked record under ANOTHER identity (a stale entry its waiter has not
/// removed yet) is never overwritten: that pid's force-stop then refuses.
/// Untracked records whose process is gone or reused are pruned here.
pub(crate) fn record_spawn_token(pid: u32, token: Option<String>) {
    let mut ids = identities();
    ids.retain(|p, r| r.tracker.is_some() || (r.token.is_some() && spawn_token(*p) == r.token));
    match (&token, ids.get(&pid)) {
        (None, _) => warn!(
            target: "tree_kill",
            "pid {pid}: its start token is unreadable, so a Doctor force-stop will refuse to              signal it"
        ),
        (Some(_), Some(r)) if r.tracker.is_some() => {
            if r.token != token {
                warn!(
                    target: "tree_kill",
                    "pid {pid}: a stale tracked record holds this pid, so a Doctor force-stop                      will refuse to signal it"
                );
            }
        }
        (Some(_), _) => {
            ids.insert(
                pid,
                Recorded {
                    token,
                    tracker: None,
                },
            );
        }
    }
}

/// Drop the [`record_spawn_identity`] record of `pid`, if it is still the one
/// recorded with `token` (a reused pid's newer record is kept). A tracker's
/// record is never dropped here.
pub(crate) fn forget_spawn_identity(pid: u32, token: Option<&str>) {
    let mut ids = identities();
    if ids
        .get(&pid)
        .is_some_and(|r| r.tracker.is_none() && r.token.as_deref() == token)
    {
        ids.remove(&pid);
    }
}

/// What one tracked-pid kill did, classified per D-4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KillOutcome {
    /// The tree kill returned `Ok` and killed at least one process.
    Killed(TreeKillReport),
    /// The tree kill returned `Ok` and killed nothing: the root had already
    /// exited (and no descendant was left).
    AlreadyExited,
    /// Nothing was signalled, for the stated reason.
    Refused(String),
}

impl KillOutcome {
    fn from_result(result: Result<TreeKillReport, String>) -> Self {
        match result {
            Ok(report) if !report.killed.is_empty() => KillOutcome::Killed(report),
            Ok(report) if report.root_exited => KillOutcome::AlreadyExited,
            Ok(report) => KillOutcome::Refused(format!(
                "the tree kill returned without killing anything ({report:?})"
            )),
            Err(e) => KillOutcome::Refused(e),
        }
    }

    /// Whether the pid is finished with: killed, exited, or refused for a
    /// reason no retry can change. A pid refused for any other reason
    /// (an unreadable `/proc` entry, a pidfd error, a spent deadline, a root
    /// that survived) may still be alive, so it goes back into its tracker
    /// rather than becoming unreachable (review r1, finding 7).
    fn is_final(&self) -> bool {
        match self {
            KillOutcome::Killed(_) | KillOutcome::AlreadyExited => true,
            KillOutcome::Refused(why) => FINAL_REFUSALS.iter().any(|p| why.contains(p)),
        }
    }
}

/// Phrases of the refusals no retry can change: the pid's process is gone or
/// is another process, or the pid can never be proven or signalled. Each is
/// pinned against the messages that carry it by `source_tests`.
const FINAL_REFUSALS: [&str; 7] = [
    " was reused",
    "no such process",
    "is not an unreaped child",
    " was reaped while",
    "has no recorded spawn identity",
    "cannot ever be proven",
    "refusing ",
];

/// The per-pid outcomes of a stop, counted per D-4.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KillTally {
    /// Pids whose tree kill returned `Ok` with a non-empty `killed`.
    pub killed: usize,
    /// Pids whose root had already exited, with nothing left to kill.
    pub already_exited: usize,
    /// Pids that were not signalled, each with its reason.
    pub refused: Vec<(u32, String)>,
}

impl KillTally {
    fn record(&mut self, label: &str, pid: u32, outcome: KillOutcome) {
        match outcome {
            KillOutcome::Killed(report) => {
                if report.budget_spent || report.recheck_snapshot_failed {
                    warn!(
                        target: "tree_kill",
                        "{label}: pid {pid}: killed {:?}, but the walk ended unfinished{}{}",
                        report.killed,
                        if report.budget_spent {
                            " (its budget or its pass cap)"
                        } else {
                            ""
                        },
                        if report.recheck_snapshot_failed {
                            " (a process could not be read, so a descendant may have been missed)"
                        } else {
                            ""
                        }
                    );
                } else {
                    info!(
                        target: "tree_kill",
                        "{label}: pid {pid}: killed its process tree {:?}",
                        report.killed
                    );
                }
                self.killed += 1;
            }
            KillOutcome::AlreadyExited => {
                info!(target: "tree_kill", "{label}: pid {pid}: had already exited; nothing killed");
                self.already_exited += 1;
            }
            KillOutcome::Refused(why) => {
                warn!(target: "tree_kill", "{label}: pid {pid}: not signalled: {why}");
                self.refused.push((pid, why));
            }
        }
    }

    /// Pids this stop looked at.
    pub fn attempted(&self) -> usize {
        self.killed + self.already_exited + self.refused.len()
    }

    /// What follows "killed N" in a reply: the exited and refused pids, when
    /// there are any. Empty otherwise.
    pub fn qualifiers(&self) -> String {
        let mut out = String::new();
        if self.already_exited > 0 {
            out.push_str(&format!("; {} had already exited", self.already_exited));
        }
        if !self.refused.is_empty() {
            let pids: Vec<String> = self.refused.iter().map(|(p, _)| p.to_string()).collect();
            out.push_str(&format!(
                "; {} not signalled (pid {})",
                self.refused.len(),
                pids.join(", ")
            ));
        }
        out
    }
}

/// The recorded identity of `pid`, or the refusal for a pid with none.
fn recorded(pid: u32) -> Result<Recorded, KillOutcome> {
    identities().get(&pid).cloned().ok_or_else(|| {
        KillOutcome::Refused(format!(
            "pid {pid} has no recorded spawn identity (it was not tracked through \
             tree_kill::track_ai_pid nor recorded through tree_kill::record_spawn_identity); \
             nothing signalled"
        ))
    })
}

/// Kill `pid`'s tree under its `recorded` identity. A token mismatch
/// ("reused") drops the stale record.
fn kill_recorded(pid: u32, recorded: &Recorded, budget: Duration) -> KillOutcome {
    // A Doctor-only record's `Child` is waited on another thread, so on every
    // OS only a token can prove the pid is still that process.
    let require_token = START_TOKENS_EXIST || recorded.tracker.is_none();
    let result = kill_verified(pid, recorded.token.as_deref(), require_token, budget);
    if matches!(&result, Err(e) if e.contains(" was reused")) {
        identities().remove(&pid);
    }
    KillOutcome::from_result(result)
}

/// Kill the tree of the tracked (or Doctor-recorded) pid `pid`, but only while
/// it is provably the process that was recorded. A pid with no recorded
/// identity is refused. A token mismatch ("reused") drops the stale record.
pub(crate) fn kill_tracked_pid(pid: u32, budget: Duration) -> KillOutcome {
    match recorded(pid) {
        Ok(r) => kill_recorded(pid, &r, budget),
        Err(refused) => refused,
    }
}

/// [`kill_tracked_pid`] off the async runtime.
pub(crate) async fn kill_tracked_pid_async(pid: u32, budget: Duration) -> KillOutcome {
    tokio::task::spawn_blocking(move || kill_tracked_pid(pid, budget))
        .await
        .unwrap_or_else(|e| KillOutcome::Refused(format!("pid {pid}: tree-kill task failed: {e}")))
}

/// [`kill_recorded`] off the async runtime.
async fn kill_recorded_async(pid: u32, recorded: Recorded, budget: Duration) -> KillOutcome {
    tokio::task::spawn_blocking(move || kill_recorded(pid, &recorded, budget))
        .await
        .unwrap_or_else(|e| KillOutcome::Refused(format!("pid {pid}: tree-kill task failed: {e}")))
}

/// Kill `pid`'s tree after proving it is the process whose start token is
/// `token`. With no token, only an OS without tokens and a pid whose `Child`
/// this runner holds unwaited (`require_token` false) proceeds.
fn kill_verified(
    pid: u32,
    token: Option<&str>,
    require_token: bool,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    if token.is_none() && require_token {
        return Err(format!(
            "pid {pid}: its start token was unreadable when it was recorded, so its identity \
             cannot ever be proven; nothing signalled"
        ));
    }
    #[cfg(windows)]
    {
        windows_kill_verified(pid, token.unwrap_or_default(), budget)
    }
    #[cfg(unix)]
    {
        unix::kill_tree(pid, token, budget)
    }
}

/// Open the process FIRST (the handle pins the pid), compare its creation
/// time with the recorded token, then walk through that same handle.
#[cfg(windows)]
fn windows_kill_verified(
    pid: u32,
    token: &str,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    use qontinui_runner_win32::process_tree::{terminate_tree_of, OwnedProcess};
    if pid == std::process::id() {
        return Err(format!(
            "pid {pid}: refusing to terminate the calling process's own tree"
        ));
    }
    /// `OpenProcess`'s answer for a pid that names no process.
    const ERROR_INVALID_PARAMETER: i32 = 87;
    let root = OwnedProcess::open(pid).map_err(|e| {
        if e.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            format!("pid {pid}: no such process ({e}); nothing signalled")
        } else {
            format!("pid {pid}: cannot open the process ({e}); nothing signalled")
        }
    })?;
    match root.creation_time().map(|c| format!("windows:{c}")) {
        Some(have) if have == token => {}
        Some(have) => {
            return Err(format!(
                "pid {pid} was reused: its start token is {have}, not the recorded {token}; \
                 nothing signalled"
            ))
        }
        None => {
            return Err(format!(
                "pid {pid}: its creation time is unreadable, so it cannot be matched to the \
                 recorded {token}; nothing signalled"
            ))
        }
    }
    terminate_tree_of(&root, pid, budget).map_err(|e| e.to_string())
}

/// After a kill of a pid taken out of `tracker`: a pid whose outcome is not
/// final goes back into the tracker, keeping its record; any other pid's
/// record is dropped.
fn settle(tracker: &Mutex<Vec<u32>>, pid: u32, taken: bool, outcome: &KillOutcome) {
    if taken && !outcome.is_final() {
        lock_tracker(tracker).push(pid);
        warn!(
            target: "tree_kill",
            "pid {pid}: not signalled for a reason that may pass; it stays tracked"
        );
    } else {
        identities().remove(&pid);
    }
}

/// Drain `tracker` and kill every tracked tree in it — "stop ALL AI work".
/// A pid refused for a reason that may pass stays tracked.
pub(crate) async fn kill_tracked(
    tracker: &Mutex<Vec<u32>>,
    budget: Duration,
    label: &str,
) -> KillTally {
    let pids = drain(tracker);
    let mut tally = KillTally::default();
    for pid in pids {
        let outcome = match recorded(pid) {
            Ok(r) => kill_recorded_async(pid, r, budget).await,
            Err(refused) => refused,
        };
        settle(tracker, pid, true, &outcome);
        tally.record(label, pid, outcome);
    }
    tally
}

/// Synchronous [`kill_tracked`] under a caller's deadline arithmetic (the
/// runner's quit). `next_budget` is asked before each pid; `None` means the
/// deadline is spent, and every pid left is reported refused, unsignalled
/// (and stays tracked).
pub(crate) fn kill_tracked_with_deadline(
    tracker: &Mutex<Vec<u32>>,
    label: &str,
    mut next_budget: impl FnMut() -> Option<Duration>,
) -> KillTally {
    let pids = drain(tracker);
    let mut tally = KillTally::default();
    let mut spent = false;
    for pid in pids {
        let outcome = match (!spent).then(&mut next_budget).flatten() {
            Some(budget) => kill_tracked_pid(pid, budget),
            None => {
                spent = true;
                KillOutcome::Refused(format!(
                    "pid {pid}: the kill deadline was spent before it was reached; nothing \
                     signalled"
                ))
            }
        };
        settle(tracker, pid, true, &outcome);
        tally.record(label, pid, outcome);
    }
    tally
}

/// Where a per-task stop finds a task run's processes: the pid of each, with
/// the start token captured when the task registered it (its `Child` then
/// held unwaited). Implemented by `claude_session::SessionManager`.
pub(crate) trait TaskPids: Sync {
    fn pids_for_task_run(&self, task_run_id: &str) -> Vec<(u32, Option<String>)>;
}

/// Kill only `pids` — one task run's processes (D-3) — and take exactly those
/// out of `tracker`; every other tracked pid stays tracked and alive.
///
/// Each pid carries the token its task registered it with, which must equal
/// the pid's recorded identity: the pid may since have been reaped and reused
/// by ANOTHER task's tracked spawn, whose record then holds the pid (review
/// r1, finding 5). A mismatched pid is refused and left in the tracker.
pub(crate) async fn kill_tracked_subset(
    tracker: &Mutex<Vec<u32>>,
    pids: &[(u32, Option<String>)],
    budget: Duration,
    label: &str,
) -> KillTally {
    let mut tally = KillTally::default();
    for (pid, registered) in pids {
        let pid = *pid;
        let checked = recorded(pid).and_then(|r| {
            if registered.is_none() && START_TOKENS_EXIST {
                Err(KillOutcome::Refused(format!(
                    "pid {pid}: its start token was unreadable when its task registered it, so \
                     it cannot ever be proven that task's process; nothing signalled"
                )))
            } else if *registered != r.token {
                Err(KillOutcome::Refused(format!(
                    "pid {pid}: its recorded identity is not the process this task run \
                     registered (the pid was reused by another spawn); nothing signalled"
                )))
            } else {
                Ok(r)
            }
        });
        let outcome = match checked {
            Ok(r) => {
                let taken = {
                    let mut t = lock_tracker(tracker);
                    let before = t.len();
                    t.retain(|p| *p != pid);
                    t.len() != before
                };
                let outcome = kill_recorded_async(pid, r, budget).await;
                settle(tracker, pid, taken, &outcome);
                outcome
            }
            Err(refused) => refused,
        };
        tally.record(label, pid, outcome);
    }
    tally
}

/// What a per-task stop did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TaskStop {
    /// No session registry was available, so the task's processes could not
    /// be looked up; nothing was signalled.
    NoSessionRegistry,
    /// The task's processes were looked up and each was handled.
    Ran(KillTally),
}

impl TaskStop {
    /// A per-task stop's reply text: what was killed, or why nothing was
    /// (never a silent "killed 0").
    pub fn summary(&self) -> String {
        match self {
            TaskStop::NoSessionRegistry => "the session registry is unavailable, so this task's \
                                            processes could not be looked up; nothing was \
                                            signalled"
                .to_string(),
            TaskStop::Ran(tally) if tally.attempted() == 0 => {
                "no process was found for this task, so nothing was signalled".to_string()
            }
            TaskStop::Ran(tally) => {
                format!("killed {} process(es){}", tally.killed, tally.qualifiers())
            }
        }
    }
}

/// THE per-task stop (D-3), shared by `POST /task-runs/{id}/stop` and the
/// GraphQL `stopTaskRun`: look up task run `task_run_id`'s processes in
/// `sessions` and kill exactly those trees.
pub(crate) async fn stop_task_run_trees(
    sessions: Option<&dyn TaskPids>,
    tracker: &Mutex<Vec<u32>>,
    task_run_id: &str,
    budget: Duration,
    label: &str,
) -> TaskStop {
    let Some(sessions) = sessions else {
        warn!(
            target: "tree_kill",
            "{label}: no session registry, so task {task_run_id}'s processes cannot be found; \
             nothing signalled"
        );
        return TaskStop::NoSessionRegistry;
    };
    let pids = sessions.pids_for_task_run(task_run_id);
    TaskStop::Ran(kill_tracked_subset(tracker, &pids, budget, label).await)
}

fn drain(tracker: &Mutex<Vec<u32>>) -> Vec<u32> {
    std::mem::take(&mut *lock_tracker(tracker))
}

#[cfg(test)]
mod source_tests;

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    fn long_child() -> Command {
        let mut c = crate::process_helpers::no_window("cmd");
        c.args(["/c", "ping", "-n", "60", "127.0.0.1"]);
        c
    }

    fn spawn_long() -> std::process::Child {
        long_child()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn exits_within(child: &mut std::process::Child, within: Duration) -> bool {
        let end = Instant::now() + within;
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn tree_kill_by_pid_ends_the_process() {
        let mut child = spawn_long();
        let report = kill_tree_by_pid(child.id(), Duration::from_secs(5)).unwrap();
        assert!(report.killed.contains(&child.id()), "{report:?}");
        assert!(exits_within(&mut child, Duration::from_secs(5)));
    }

    #[test]
    fn tree_kill_of_a_child_ends_it() {
        let mut child = spawn_long();
        let report = kill_child_tree(&child, Duration::from_secs(5)).unwrap();
        assert!(report.killed.contains(&child.id()), "{report:?}");
        assert!(exits_within(&mut child, Duration::from_secs(5)));
    }

    #[test]
    fn tree_kill_of_a_missing_pid_is_an_error_naming_it() {
        // Far above any pid Windows allocates.
        let err = kill_tree_by_pid(2_147_483_647, Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("2147483647"), "{err}");
    }

    #[test]
    fn tree_kill_refuses_the_runner_itself() {
        let err = kill_tree_by_pid(std::process::id(), Duration::from_secs(1)).unwrap_err();
        assert!(err.contains("refusing"), "{err}");
    }

    /// T8 twin.
    #[test]
    fn tree_kill_tracked_ends_each_tracked_tree_and_empties_the_tracker() {
        let tracker = Mutex::new(Vec::new());
        let mut a = spawn_long();
        let mut b = spawn_long();
        track_ai_pid(&tracker, a.id());
        track_ai_pid(&tracker, b.id());
        let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(5), "T8"));
        assert_eq!(tally.killed, 2, "{tally:?}");
        assert!(lock_tracker(&tracker).is_empty());
        assert!(exits_within(&mut a, Duration::from_secs(5)));
        assert!(exits_within(&mut b, Duration::from_secs(5)));
    }

    /// T9 twin: a tracked pid whose process is gone and whose handle was
    /// released (the Windows shape of "reaped") is refused, not counted.
    #[test]
    fn tree_kill_tracked_counts_only_kills() {
        let tracker = Mutex::new(Vec::new());
        let mut live = spawn_long();
        let mut gone = crate::process_helpers::no_window("cmd")
            .args(["/c", "exit", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        track_ai_pid(&tracker, live.id());
        track_ai_pid(&tracker, gone.id());
        let gone_pid = gone.id();
        gone.wait().unwrap();
        drop(gone);
        let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(5), "T9"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert_eq!(tally.attempted(), 2, "{tally:?}");
        assert!(
            tally.already_exited == 1 || tally.refused.iter().any(|(p, _)| *p == gone_pid),
            "{tally:?}"
        );
        assert!(exits_within(&mut live, Duration::from_secs(5)));
    }

    /// T9b twin: a root that exited while its `Child` (handle) is still held
    /// is `already_exited`, never counted as killed.
    #[test]
    fn tree_kill_an_exited_unwaited_root_is_already_exited_not_killed() {
        let tracker = Mutex::new(Vec::new());
        // `set /p` waits on stdin, so the process is certainly alive when it
        // is tracked; closing stdin then lets it exit.
        let mut child = crate::process_helpers::no_window("cmd")
            .args(["/c", "set /p x="])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        track_ai_pid(&tracker, child.id());
        drop(child.stdin.take());
        // Let it exit, keeping the handle: `try_wait` is not called, so the
        // process object (and the pid) stays pinned by the `Child`.
        let deadline = Instant::now() + Duration::from_secs(10);
        while qontinui_pty_holder::proc_identity::start_token(child.id()).is_some() {
            assert!(Instant::now() < deadline, "cmd did not exit on EOF");
            std::thread::sleep(Duration::from_millis(20));
        }
        let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(5), "T9b"));
        assert_eq!(tally.killed, 0, "{tally:?}");
        assert_eq!(tally.already_exited, 1, "{tally:?}");
        child.wait().unwrap();
    }

    /// T10 twin.
    #[test]
    fn tree_kill_tracked_on_an_empty_tracker_kills_nothing_repeatedly() {
        let tracker = Mutex::new(Vec::new());
        for _ in 0..2 {
            let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(1), "T10"));
            assert_eq!(tally, KillTally::default());
        }
    }

    /// T14 twin.
    #[test]
    fn tree_kill_an_untracked_pid_is_refused() {
        let mut child = spawn_long();
        let outcome = kill_tracked_pid(child.id(), Duration::from_secs(5));
        assert!(
            matches!(&outcome, KillOutcome::Refused(why) if why.contains("no recorded spawn identity")),
            "{outcome:?}"
        );
        assert!(
            !exits_within(&mut child, Duration::from_millis(300)),
            "child was killed"
        );
        kill_child_tree(&child, Duration::from_secs(5)).unwrap();
        child.wait().unwrap();
    }

    /// T15 twin: the per-task helper kills task A's tree and leaves task B's
    /// alive AND tracked.
    #[test]
    fn tree_kill_per_task_scope_leaves_another_tasks_tree_alive_and_tracked() {
        let tracker = Mutex::new(Vec::new());
        let mut a = spawn_long();
        let mut b = spawn_long();
        let ta = track_ai_pid(&tracker, a.id());
        track_ai_pid(&tracker, b.id());
        let tally = rt().block_on(kill_tracked_subset(
            &tracker,
            &[(a.id(), ta)],
            Duration::from_secs(5),
            "T15",
        ));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert!(exits_within(&mut a, Duration::from_secs(5)));
        assert_eq!(*lock_tracker(&tracker), vec![b.id()]);
        assert!(
            !exits_within(&mut b, Duration::from_millis(300)),
            "task B was killed"
        );
        let tally = rt().block_on(kill_tracked(
            &tracker,
            Duration::from_secs(5),
            "T15 cleanup",
        ));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert!(exits_within(&mut b, Duration::from_secs(5)));
    }

    /// Review r1, finding 5: a task's pid whose recorded identity is now
    /// ANOTHER spawn's (the pid was reaped and reused) is refused, and that
    /// spawn stays alive and tracked.
    #[test]
    fn tree_kill_per_task_refuses_a_pid_another_spawn_now_holds() {
        let tracker = Mutex::new(Vec::new());
        let mut b = spawn_long();
        track_ai_pid(&tracker, b.id());
        let stale = Some("windows:1".to_string());
        let tally = rt().block_on(kill_tracked_subset(
            &tracker,
            &[(b.id(), stale)],
            Duration::from_secs(5),
            "finding 5",
        ));
        assert_eq!(tally.killed, 0, "{tally:?}");
        assert!(
            tally
                .refused
                .iter()
                .any(|(p, why)| *p == b.id() && why.contains("reused")),
            "{tally:?}"
        );
        assert_eq!(*lock_tracker(&tracker), vec![b.id()]);
        assert!(
            !exits_within(&mut b, Duration::from_millis(300)),
            "the other spawn was killed"
        );
        let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(5), "cleanup"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert!(exits_within(&mut b, Duration::from_secs(5)));
    }

    /// Review r1, finding 7: a pid refused for a reason that may pass (here
    /// a spent deadline) stays tracked with its identity, so a later stop
    /// still kills it.
    #[test]
    fn tree_kill_a_transiently_refused_pid_stays_tracked() {
        let tracker = Mutex::new(Vec::new());
        let mut a = spawn_long();
        track_ai_pid(&tracker, a.id());
        let tally = kill_tracked_with_deadline(&tracker, "finding 7", || None);
        assert_eq!(tally.refused.len(), 1, "{tally:?}");
        assert_eq!(*lock_tracker(&tracker), vec![a.id()]);
        let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(5), "retry"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert!(exits_within(&mut a, Duration::from_secs(5)));
    }

    /// Review r1, finding 1: a process recorded through
    /// `record_spawn_identity` (Doctor-listed, in no tracker) is killed by
    /// the force-stop; once forgotten it is refused again.
    #[test]
    fn tree_kill_a_doctor_recorded_pid_is_killable_until_forgotten() {
        let mut a = spawn_long();
        let token = record_spawn_identity(a.id());
        assert!(token.is_some());
        forget_spawn_identity(a.id(), token.as_deref());
        let outcome = kill_tracked_pid(a.id(), Duration::from_secs(5));
        assert!(
            matches!(&outcome, KillOutcome::Refused(why) if why.contains("no recorded spawn identity")),
            "{outcome:?}"
        );
        assert!(!exits_within(&mut a, Duration::from_millis(300)), "killed");
        record_spawn_identity(a.id());
        let outcome = kill_tracked_pid(a.id(), Duration::from_secs(5));
        assert!(matches!(outcome, KillOutcome::Killed(_)), "{outcome:?}");
        assert!(exits_within(&mut a, Duration::from_secs(5)));
    }

    /// A [`TaskPids`] over fixed rows, standing in for the SessionManager.
    struct Rows(Vec<(&'static str, u32, Option<String>)>);

    impl TaskPids for Rows {
        fn pids_for_task_run(&self, id: &str) -> Vec<(u32, Option<String>)> {
            self.0
                .iter()
                .filter(|(t, _, _)| *t == id)
                .map(|(_, p, k)| (*p, k.clone()))
                .collect()
        }
    }

    /// Review r1, finding 8: THE per-task stop both endpoints call kills the
    /// named task's tree only, and without a session registry signals
    /// nothing and says so.
    #[test]
    fn tree_kill_stop_task_run_trees_kills_only_that_task() {
        let tracker = Mutex::new(Vec::new());
        let mut a = spawn_long();
        let mut b = spawn_long();
        let ta = track_ai_pid(&tracker, a.id());
        let tb = track_ai_pid(&tracker, b.id());
        let rows = Rows(vec![("A", a.id(), ta), ("B", b.id(), tb)]);
        let none = rt().block_on(stop_task_run_trees(
            None,
            &tracker,
            "A",
            Duration::from_secs(5),
            "finding 8",
        ));
        assert_eq!(none, TaskStop::NoSessionRegistry);
        assert!(none.summary().contains("nothing was signalled"));
        assert_eq!(lock_tracker(&tracker).len(), 2);
        let stop = rt().block_on(stop_task_run_trees(
            Some(&rows as &dyn TaskPids),
            &tracker,
            "A",
            Duration::from_secs(5),
            "finding 8",
        ));
        assert!(
            matches!(&stop, TaskStop::Ran(t) if t.killed == 1),
            "{stop:?}"
        );
        assert!(exits_within(&mut a, Duration::from_secs(5)));
        assert_eq!(*lock_tracker(&tracker), vec![b.id()]);
        assert!(
            !exits_within(&mut b, Duration::from_millis(300)),
            "task B was killed"
        );
        let tally = rt().block_on(kill_tracked(&tracker, Duration::from_secs(5), "cleanup"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert!(exits_within(&mut b, Duration::from_secs(5)));
    }
}
