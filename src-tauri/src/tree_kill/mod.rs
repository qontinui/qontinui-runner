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
//! is refused, never signalled.
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

/// What [`track_ai_pid`] recorded for one pid.
#[derive(Debug, Clone)]
struct Recorded {
    /// The pid's start token at track time; `None` when unreadable, or on an
    /// OS with no token.
    token: Option<String>,
    /// The tracker the pid was pushed into (its address), so pruning one
    /// tracker's departed pids never touches another tracker's entries.
    tracker: usize,
}

/// Spawn identities of tracked pids, keyed by pid.
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

/// THE writer of an AI pid tracker. Push `pid` — a child this runner has just
/// spawned and not yet waited, so the pid certainly names it — and record its
/// start token, against which every later kill of the pid is checked.
///
/// Entries of this tracker whose pid has since left it (its waiter removed
/// it) are pruned here, so the record stays as small as the tracker.
pub(crate) fn track_ai_pid(tracker: &Mutex<Vec<u32>>, pid: u32) {
    let token = qontinui_pty_holder::proc_identity::start_token(pid);
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
    ids.retain(|p, r| r.tracker != key || pids.contains(p));
    ids.insert(
        pid,
        Recorded {
            token,
            tracker: key,
        },
    );
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
}

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

/// Kill the tree of the tracked pid `pid`, but only while it is provably the
/// process [`track_ai_pid`] recorded. A pid with no recorded identity is
/// refused. A token mismatch ("reused") drops the stale record.
pub(crate) fn kill_tracked_pid(pid: u32, budget: Duration) -> KillOutcome {
    let recorded = identities().get(&pid).cloned();
    let Some(recorded) = recorded else {
        return KillOutcome::Refused(format!(
            "pid {pid} has no recorded spawn identity (it was not tracked through \
             tree_kill::track_ai_pid); nothing signalled"
        ));
    };
    let result = kill_verified(pid, recorded.token.as_deref(), budget);
    if matches!(&result, Err(e) if e.contains(" was reused")) {
        identities().remove(&pid);
    }
    KillOutcome::from_result(result)
}

/// [`kill_tracked_pid`] off the async runtime.
pub(crate) async fn kill_tracked_pid_async(pid: u32, budget: Duration) -> KillOutcome {
    tokio::task::spawn_blocking(move || kill_tracked_pid(pid, budget))
        .await
        .unwrap_or_else(|e| KillOutcome::Refused(format!("pid {pid}: tree-kill task failed: {e}")))
}

/// Kill `pid`'s tree after proving it is the process whose start token is
/// `token`.
fn kill_verified(
    pid: u32,
    token: Option<&str>,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    if token.is_none() && START_TOKENS_EXIST {
        return Err(format!(
            "pid {pid}: its start token was unreadable when it was tracked, so its identity \
             cannot be proven; nothing signalled"
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
    let root = OwnedProcess::open(pid)
        .map_err(|e| format!("pid {pid}: cannot open the process ({e}); nothing signalled"))?;
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

/// Drain `tracker` and kill every tracked tree in it — "stop ALL AI work".
pub(crate) async fn kill_tracked(
    tracker: &Mutex<Vec<u32>>,
    budget: Duration,
    label: &str,
) -> KillTally {
    let pids = drain(tracker);
    let mut tally = KillTally::default();
    for pid in pids {
        let outcome = kill_tracked_pid_async(pid, budget).await;
        identities().remove(&pid);
        tally.record(label, pid, outcome);
    }
    tally
}

/// Synchronous [`kill_tracked`] under a caller's deadline arithmetic (the
/// runner's quit). `next_budget` is asked before each pid; `None` means the
/// deadline is spent, and every pid left is reported refused, unsignalled.
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
        identities().remove(&pid);
        tally.record(label, pid, outcome);
    }
    tally
}

/// Kill only `pids` — one task run's processes (D-3) — and remove exactly
/// those from `tracker`; every other tracked pid stays tracked and alive.
pub(crate) async fn kill_tracked_subset(
    tracker: &Mutex<Vec<u32>>,
    pids: &[u32],
    budget: Duration,
    label: &str,
) -> KillTally {
    lock_tracker(tracker).retain(|p| !pids.contains(p));
    let mut tally = KillTally::default();
    for &pid in pids {
        let outcome = kill_tracked_pid_async(pid, budget).await;
        identities().remove(&pid);
        tally.record(label, pid, outcome);
    }
    tally
}

/// A per-task stop's reply text: what was killed, or that no process was
/// found for the task (never a silent "killed 0").
pub(crate) fn task_stop_summary(tally: &KillTally) -> String {
    if tally.attempted() == 0 {
        "no process was found for this task, so nothing was signalled".to_string()
    } else {
        format!("killed {} process(es){}", tally.killed, tally.qualifiers())
    }
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
        track_ai_pid(&tracker, a.id());
        track_ai_pid(&tracker, b.id());
        let tally = rt().block_on(kill_tracked_subset(
            &tracker,
            &[a.id()],
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
}
