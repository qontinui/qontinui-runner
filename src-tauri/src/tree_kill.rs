//! Kill a process tree WITHOUT creating a process (Windows).
//!
//! Plan `2026-10-19-served-corpus-probe-budget-excludes-process-creation`,
//! Phase 4. Every runner kill path on Windows used to spawn
//! `taskkill /F /T /PID`. That is itself a process creation, and on Windows
//! Rust std serializes every `std::process` spawn behind one process-wide
//! `CreateProcessW` lock, so a kill could wait as long as the slowest spawn
//! queued ahead of it. These helpers end the tree in-process instead, through
//! the Toolhelp walk in [`qontinui_runner_win32::process_tree`]: the root and
//! every descendant linked to it through a parent this walk holds a handle
//! on, with a pid-reuse guard (a process created before its recorded parent is
//! never admitted).
//!
//! Windows-only, like the `taskkill` it replaces: the call sites that are not
//! `cfg(windows)` keep their non-Windows behaviour (they never killed anything
//! there — the `taskkill` spawn failed), which is a separate defect, recorded
//! as a coord finding rather than changed here.
//!
//! [`kill_tree_by_pid`] takes a bare pid and is only as good as the moment
//! the pid was read. [`kill_child_tree`] roots the walk at a `Child`'s own
//! handle, which pins the identity for as long as the `Child` is alive. Both
//! refuse the runner's own pid.

use std::time::Duration;

pub(crate) use qontinui_runner_win32::process_tree::TreeKillReport;

/// End `pid` and its descendants. `Err` names the pid when it cannot be
/// opened (it already exited, or is not ours), when it is the runner itself,
/// or when it survives the kill.
pub(crate) fn kill_tree_by_pid(pid: u32, budget: Duration) -> Result<TreeKillReport, String> {
    qontinui_runner_win32::process_tree::terminate_process_tree(pid, budget)
        .map_err(|e| e.to_string())
}

/// [`kill_tree_by_pid`] off the async runtime: the walk is short, but it is
/// blocking Win32 work, and a runtime worker must not do it.
pub(crate) async fn kill_tree_by_pid_async(
    pid: u32,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    tokio::task::spawn_blocking(move || kill_tree_by_pid(pid, budget))
        .await
        .map_err(|e| format!("pid {pid}: tree-kill task failed: {e}"))?
}

/// End `child` and its descendants, rooted at the child's own handle.
pub(crate) fn kill_child_tree(
    child: &std::process::Child,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    use std::os::windows::io::AsHandle;
    let root = qontinui_runner_win32::process_tree::OwnedProcess::duplicate(child.as_handle())
        .map_err(|e| format!("pid {}: cannot duplicate the child handle: {e}", child.id()))?;
    qontinui_runner_win32::process_tree::terminate_tree_of(&root, child.id(), budget)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    fn long_child() -> Command {
        let mut c = crate::process_helpers::no_window("cmd");
        c.args(["/c", "ping", "-n", "60", "127.0.0.1"]);
        c
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

    #[test]
    fn tree_kill_by_pid_ends_the_process() {
        let mut child = long_child()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let report = kill_tree_by_pid(child.id(), Duration::from_secs(5)).unwrap();
        assert!(report.killed.contains(&child.id()), "{report:?}");
        assert!(exits_within(&mut child, Duration::from_secs(5)));
    }

    #[test]
    fn tree_kill_of_a_child_ends_it() {
        let mut child = long_child()
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
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
}
