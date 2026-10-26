//! The Unix arm of [`crate::tree_kill`] (plan
//! `2026-10-25-runner-kill-paths-never-kill-anything-off-windows`, Phase 2).
//!
//! - **Linux**: `qontinui_pty_holder::tree_freeze::kill_tree`, the freeze
//!   walk. The root must be an unreaped child of this runner; an expected
//!   start token is compared inside its pidfd window; every member is stopped
//!   before it is admitted and killed after the walk, so no stranger is ever
//!   signalled. This file adds no walk of its own.
//! - **Other Unixes (macOS)**: no `/proc` and no pidfd. D-1's rule: the
//!   `waitid` proof that the root is an unreaped child (and a start token,
//!   where the OS has one), then `SIGSTOP` the root, then `killpg` when the
//!   root leads its own group (one other than the runner's), else `kill` the
//!   root alone. Tracked AI pids are never group leaders, so in practice this
//!   arm is root-only. CI builds no macOS leg, so it is COMPILE-UNVERIFIED.

use std::time::Duration;

use super::TreeKillReport;

/// Kill `pid` and, where the OS allows it to be reached safely, its tree.
/// `expected_token` is the pid's start token recorded at spawn; a mismatch
/// signals nothing ("reused").
#[cfg(target_os = "linux")]
pub(super) fn kill_tree(
    pid: u32,
    expected_token: Option<&str>,
    budget: Duration,
) -> Result<TreeKillReport, String> {
    qontinui_pty_holder::tree_freeze::kill_tree(pid, expected_token, budget)
        .map_err(|e| e.to_string())
}

/// D-1's non-Linux rule. Compile-unverified (no CI leg builds it).
#[cfg(not(target_os = "linux"))]
pub(super) fn kill_tree(
    pid: u32,
    expected_token: Option<&str>,
    _budget: Duration,
) -> Result<TreeKillReport, String> {
    let Some(spid) = libc::pid_t::try_from(pid).ok().filter(|p| *p > 1) else {
        return Err(format!(
            "pid {pid}: refusing a pid outside 2..=i32::MAX; nothing signalled"
        ));
    };
    if pid == std::process::id() {
        return Err(format!(
            "pid {pid}: refusing to kill this process's own tree; nothing signalled"
        ));
    }
    // SAFETY: a zeroed siginfo_t is a valid out-parameter for waitid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid, writable siginfo_t; WNOWAIT leaves the child
    // unreaped, WNOHANG keeps the call from blocking.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            spid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(format!(
            "pid {pid} is not an unreaped child of this runner ({e}); nothing signalled"
        ));
    }
    if info.si_pid == spid {
        return Ok(TreeKillReport {
            root_exited: true,
            ..TreeKillReport::default()
        });
    }
    if let Some(want) = expected_token {
        match qontinui_pty_holder::proc_identity::start_token(pid) {
            Some(have) if have == want => {}
            have => {
                return Err(format!(
                    "pid {pid} was reused: its start token is {have:?}, not the recorded \
                     {want}; nothing signalled"
                ))
            }
        }
    }
    // SAFETY: plain signal syscalls on a pid proven above to be our unreaped
    // child, which cannot be reused while it stays unreaped.
    unsafe {
        libc::kill(spid, libc::SIGSTOP);
    }
    // SAFETY: as above; getpgid takes no pointers.
    let (group, own) = unsafe { (libc::getpgid(spid), libc::getpgid(0)) };
    // SAFETY: as above.
    let rc = unsafe {
        if group == spid && group != own {
            libc::killpg(group, libc::SIGKILL)
        } else {
            libc::kill(spid, libc::SIGKILL)
        }
    };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        // SAFETY: as above; resume the root we stopped.
        unsafe {
            libc::kill(spid, libc::SIGCONT);
        }
        return Err(format!("pid {pid}: SIGKILL failed ({e})"));
    }
    Ok(TreeKillReport {
        killed: vec![pid],
        passes: 1,
        ..TreeKillReport::default()
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::super::*;
    use qontinui_pty_holder::proc_identity;
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    /// `sh` with a `sleep` child: a two-level tree. Returns the child and the
    /// pid of its `sleep`.
    fn spawn_tree() -> (Child, u32) {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 300 & echo $!; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sh");
        let mut line = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let grandchild: u32 = line.trim().parse().expect("sleep pid");
        (child, grandchild)
    }

    /// `(pid, start ticks)` so "gone" can tell a reused pid from the process.
    fn identity(pid: u32) -> (u32, u64) {
        let st = proc_identity::stat(pid).expect("live process has a stat");
        (pid, st.start_ticks)
    }

    fn is_gone((pid, ticks): (u32, u64)) -> bool {
        match proc_identity::stat(pid) {
            None => true,
            Some(st) => st.zombie || st.start_ticks != ticks,
        }
    }

    /// Alive, the same process, and not left stopped by a kill attempt.
    fn is_running((pid, ticks): (u32, u64)) -> bool {
        proc_identity::stat(pid)
            .is_some_and(|st| !st.zombie && st.start_ticks == ticks && st.state != b'T')
    }

    fn gone_within(id: (u32, u64), within: Duration) -> bool {
        let end = Instant::now() + within;
        loop {
            if is_gone(id) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run `f` to completion on a fresh current-thread runtime (built
    /// inline: clippy.toml disallows naming an owned `Runtime`).
    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// Kill a test's own tree and reap its root, bounded.
    fn cleanup(mut child: Child) {
        let _ = kill_child_tree(&child, Duration::from_secs(5));
        let end = Instant::now() + Duration::from_secs(5);
        while !matches!(child.try_wait(), Ok(Some(_))) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn kill_tracked_ends_each_tracked_tree_and_empties_the_tracker() {
        let tracker = Mutex::new(Vec::new());
        let (mut a, ga) = spawn_tree();
        let (mut b, gb) = spawn_tree();
        let ids = [
            identity(a.id()),
            identity(ga),
            identity(b.id()),
            identity(gb),
        ];
        track_ai_pid(&tracker, a.id());
        track_ai_pid(&tracker, b.id());
        let tally = block_on(kill_tracked(&tracker, Duration::from_secs(5), "T8"));
        assert_eq!(tally.killed, 2, "{tally:?}");
        assert!(lock_tracker(&tracker).is_empty());
        a.wait().unwrap();
        b.wait().unwrap();
        for id in ids {
            assert!(gone_within(id, Duration::from_secs(5)), "{id:?} survived");
        }
    }

    #[test]
    fn kill_tracked_counts_only_kills() {
        let tracker = Mutex::new(Vec::new());
        let (mut live, g) = spawn_tree();
        let ids = [identity(live.id()), identity(g)];
        let mut reaped = Command::new("sh")
            .args(["-c", "read x"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        track_ai_pid(&tracker, live.id());
        track_ai_pid(&tracker, reaped.id());
        let reaped_pid = reaped.id();
        drop(reaped.stdin.take());
        reaped.wait().unwrap();
        let tally = block_on(kill_tracked(&tracker, Duration::from_secs(5), "T9"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        assert_eq!(tally.already_exited, 0, "{tally:?}");
        assert!(
            tally.refused.iter().any(|(p, _)| *p == reaped_pid),
            "{tally:?}"
        );
        live.wait().unwrap();
        for id in ids {
            assert!(gone_within(id, Duration::from_secs(5)), "{id:?} survived");
        }
    }

    #[test]
    fn an_exited_unwaited_root_is_already_exited_not_killed() {
        let tracker = Mutex::new(Vec::new());
        let mut child = Command::new("sh")
            .args(["-c", "read x"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        track_ai_pid(&tracker, child.id());
        drop(child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !proc_identity::stat(child.id()).is_some_and(|st| st.zombie) {
            assert!(Instant::now() < deadline, "sh did not exit on EOF");
            std::thread::sleep(Duration::from_millis(20));
        }
        let tally = block_on(kill_tracked(&tracker, Duration::from_secs(5), "T9b"));
        assert_eq!(tally.killed, 0, "{tally:?}");
        assert_eq!(tally.already_exited, 1, "{tally:?}");
        child.wait().unwrap();
    }

    #[test]
    fn kill_tracked_on_an_empty_tracker_kills_nothing_repeatedly() {
        let tracker = Mutex::new(Vec::new());
        for _ in 0..2 {
            let tally = block_on(kill_tracked(&tracker, Duration::from_secs(1), "T10"));
            assert_eq!(tally, KillTally::default());
        }
    }

    #[test]
    fn an_untracked_pid_is_refused() {
        let (child, g) = spawn_tree();
        let ids = [identity(child.id()), identity(g)];
        let outcome = kill_tracked_pid(child.id(), Duration::from_secs(5));
        assert!(
            matches!(&outcome, KillOutcome::Refused(why) if why.contains("no recorded spawn identity")),
            "{outcome:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
        for id in ids {
            assert!(is_running(id), "{id:?} was killed or left stopped");
        }
        cleanup(child);
    }

    #[test]
    fn per_task_scope_leaves_another_tasks_tree_alive_and_tracked() {
        let tracker = Mutex::new(Vec::new());
        let (mut a, ga) = spawn_tree();
        let (b, gb) = spawn_tree();
        let a_ids = [identity(a.id()), identity(ga)];
        let b_ids = [identity(b.id()), identity(gb)];
        let ta = track_ai_pid(&tracker, a.id());
        track_ai_pid(&tracker, b.id());
        let tally = block_on(kill_tracked_subset(
            &tracker,
            &[(a.id(), ta)],
            Duration::from_secs(5),
            "T15",
        ));
        assert_eq!(tally.killed, 1, "{tally:?}");
        a.wait().unwrap();
        for id in a_ids {
            assert!(gone_within(id, Duration::from_secs(5)), "{id:?} survived");
        }
        assert_eq!(*lock_tracker(&tracker), vec![b.id()]);
        std::thread::sleep(Duration::from_millis(200));
        for id in b_ids {
            assert!(is_running(id), "task B's {id:?} was killed or left stopped");
        }
        let tally = block_on(kill_tracked(
            &tracker,
            Duration::from_secs(5),
            "T15 cleanup",
        ));
        assert_eq!(tally.killed, 1, "{tally:?}");
        cleanup(b);
    }

    /// Review r1, finding 5: a task's pid whose recorded identity is now
    /// ANOTHER spawn's (the pid was reaped and reused) is refused, and that
    /// spawn stays alive and tracked.
    #[test]
    fn per_task_refuses_a_pid_another_spawn_now_holds() {
        let tracker = Mutex::new(Vec::new());
        let (b, gb) = spawn_tree();
        let ids = [identity(b.id()), identity(gb)];
        track_ai_pid(&tracker, b.id());
        let stale = Some("linux:1".to_string());
        let tally = block_on(kill_tracked_subset(
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
        std::thread::sleep(Duration::from_millis(200));
        for id in ids {
            assert!(is_running(id), "{id:?} was killed or left stopped");
        }
        let tally = block_on(kill_tracked(&tracker, Duration::from_secs(5), "cleanup"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        cleanup(b);
    }

    /// Review r1, finding 7: a pid refused for a reason that may pass (here
    /// a spent deadline) stays tracked with its identity, so a later stop
    /// still kills it.
    #[test]
    fn a_transiently_refused_pid_stays_tracked() {
        let tracker = Mutex::new(Vec::new());
        let (mut a, ga) = spawn_tree();
        let ids = [identity(a.id()), identity(ga)];
        track_ai_pid(&tracker, a.id());
        let tally = kill_tracked_with_deadline(&tracker, "finding 7", || None);
        assert_eq!(tally.refused.len(), 1, "{tally:?}");
        assert_eq!(*lock_tracker(&tracker), vec![a.id()]);
        let tally = block_on(kill_tracked(&tracker, Duration::from_secs(5), "retry"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        a.wait().unwrap();
        for id in ids {
            assert!(gone_within(id, Duration::from_secs(5)), "{id:?} survived");
        }
    }

    /// Review r1, finding 1: a process recorded through
    /// `record_spawn_identity` (Doctor-listed, in no tracker) is killed by
    /// the force-stop; once forgotten it is refused again.
    #[test]
    fn a_doctor_recorded_pid_is_killable_until_forgotten() {
        let (mut a, ga) = spawn_tree();
        let ids = [identity(a.id()), identity(ga)];
        let token = record_spawn_identity(a.id());
        assert!(token.is_some());
        forget_spawn_identity(a.id(), token.as_deref());
        let outcome = kill_tracked_pid(a.id(), Duration::from_secs(5));
        assert!(
            matches!(&outcome, KillOutcome::Refused(why) if why.contains("no recorded spawn identity")),
            "{outcome:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
        for id in ids {
            assert!(is_running(id), "{id:?} was killed or left stopped");
        }
        record_spawn_identity(a.id());
        let outcome = kill_tracked_pid(a.id(), Duration::from_secs(5));
        assert!(matches!(outcome, KillOutcome::Killed(_)), "{outcome:?}");
        a.wait().unwrap();
        for id in ids {
            assert!(gone_within(id, Duration::from_secs(5)), "{id:?} survived");
        }
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
    fn stop_task_run_trees_kills_only_that_task() {
        let tracker = Mutex::new(Vec::new());
        let (mut a, ga) = spawn_tree();
        let (b, gb) = spawn_tree();
        let a_ids = [identity(a.id()), identity(ga)];
        let b_ids = [identity(b.id()), identity(gb)];
        let ta = track_ai_pid(&tracker, a.id());
        let tb = track_ai_pid(&tracker, b.id());
        let rows = Rows(vec![("A", a.id(), ta), ("B", b.id(), tb)]);
        let none = block_on(stop_task_run_trees(
            None,
            &tracker,
            "A",
            Duration::from_secs(5),
            "finding 8",
        ));
        assert_eq!(none, TaskStop::NoSessionRegistry);
        assert!(none.summary().contains("nothing was signalled"));
        assert_eq!(lock_tracker(&tracker).len(), 2);
        let stop = block_on(stop_task_run_trees(
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
        a.wait().unwrap();
        for id in a_ids {
            assert!(gone_within(id, Duration::from_secs(5)), "{id:?} survived");
        }
        assert_eq!(*lock_tracker(&tracker), vec![b.id()]);
        std::thread::sleep(Duration::from_millis(200));
        for id in b_ids {
            assert!(is_running(id), "task B's {id:?} was killed or left stopped");
        }
        let tally = block_on(kill_tracked(&tracker, Duration::from_secs(5), "cleanup"));
        assert_eq!(tally.killed, 1, "{tally:?}");
        cleanup(b);
    }
}
