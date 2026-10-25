//! Reaping a pane's process tree — the job the runner's `KILL_ON_JOB_CLOSE`
//! Job Object did for an in-process PTY, moved into the holder (plan D8).
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3. An out-of-process pane reports `pid() == None` to the runner, which
//! silently disables the runner's Job Object assignment; so crash-safety
//! reaping is the HOLDER's job, and under D13 each holder reaps exactly its own
//! child tree. Two moments, two entry points:
//!
//! 1. **The pane is KILLED** (a tab closed, or the runner gave up on it —
//!    `crate::pty::Pane::kill_now`). Its holder ends what the child leaves:
//!    - [`reap_session_of_exited_child`] — called by the holder's waiter the
//!      moment the killed child has exited and BEFORE it reaps it. The child is
//!      then a zombie, so its pid — which is its session and process-group id
//!      (it `setsid()`s onto its PTY) — cannot be reused by anyone: "session
//!      == child pid" provably names the pane's session. Linux: every member
//!      of that session; other Unix: `killpg` of the child's group. Done later
//!      (after the reap), an emptied session's id could already belong to a
//!      stranger that called `setsid()`.
//!    - [`reap_own_scope`] — Linux scope route, when the pane ends, still under
//!      the pane lock: every other process in the holder's own transient
//!      scope cgroup, which also catches a descendant that `setsid()`ed out of
//!      the session. The cgroup is the holder's own, so it names nothing else.
//!    - Windows: the holder-owned job (`crate::pty`) is terminated, which ends
//!      every process in it; this module is not involved.
//!
//!    A child that EXITS ON ITS OWN is not followed by any reap (decision of
//!    2026-10-14): what it left running — `nohup` / `disown` jobs, a tmux
//!    server, an agent — keeps running, exactly as with an in-process PTY.
//!    D8's reaping is crash safety, the job the runner's `KILL_ON_JOB_CLOSE`
//!    did when its OWNER went away; on Windows that stays true as-is (the
//!    holder's job ends its contents when the holder exits).
//!
//! 2. **A holder died without doing that** ([`reap_orphans_of_dead_holder`],
//!    called by the runner's boot sweep, `crate::sweep`, holding the dead
//!    holder's lock). The holder's death already hung up the PTY (Unix: the
//!    session's foreground group got `SIGHUP`) or closed its job (Windows:
//!    everything in it ended). What can survive is a `SIGHUP`-immune remainder
//!    on Unix, or a child the holder could not put in its job on Windows. The
//!    sweep ends it only where identity is PROVEN:
//!    - the recorded child is alive and its start token matches the lock
//!      record's (`crate::proc_identity`) → its whole tree is ended (Linux:
//!      its session; Windows: it and its descendants, each created no earlier
//!      than its parent);
//!    - the record names the holder's transient scope unit (Linux scope route)
//!      → that unit is stopped; a unit name is unique per spawn attempt, so it
//!      can name nothing else;
//!    - otherwise NOTHING is signalled, and what is left is reported as
//!      unattributed. Once the leader is gone the session id it pinned is no
//!      longer provably the pane's, and a reaper that guesses kills strangers.
//!
//! **Every signal goes through a handle taken BEFORE the identity check**
//! (Linux `pidfd_open`, Windows `OpenProcess`), exactly as `crate::terminate`
//! does: from the handle on, the check and the kill are about one process.
//! Where `pidfd_open` is missing (Linux < 5.3) the check is followed by a plain
//! `kill(pid)`, and the report says so.

use crate::lock::LockRecord;

/// Most passes a reaper makes over a session or cgroup. A pass that finds a
/// member a previous pass already signalled skips it; new members (forked
/// between passes) are caught by the next pass. Bounded, so a fork bomb in the
/// pane cannot hold the holder (or the sweep) forever.
pub const REAP_PASSES: usize = 10;

/// What a reaper did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReapReport {
    /// Pids a kill was delivered to, each after its identity was verified.
    pub killed: Vec<u32>,
    /// Processes left alone because nothing proves they belong to the pane.
    pub unattributed: Vec<u32>,
    /// What ran, and anything that could not — one line each.
    pub notes: Vec<String>,
    /// The process snapshot could not be read, so `unattributed` is UNKNOWN,
    /// not empty: the caller must keep whatever it would drop on an empty one.
    pub snapshot_unreadable: bool,
}

impl ReapReport {
    pub fn note(&mut self, line: impl Into<String>) {
        self.notes.push(line.into());
    }

    /// One line for a log.
    pub fn summary(&self) -> String {
        format!(
            "killed={:?} unattributed={:?} notes=[{}]",
            self.killed,
            self.unattributed,
            self.notes.join("; ")
        )
    }
}

/// Holder side: end what the child left in its session. **Only sound while
/// `child_pid` is not yet reaped** (alive, or a zombie), which is what pins its
/// session id — see the module docs, moment 1. Never signals the calling
/// process.
#[cfg(unix)]
pub fn reap_session_of_exited_child(child_pid: u32) -> ReapReport {
    let mut report = ReapReport::default();
    match crate::spawn::signalable_pid(child_pid) {
        Some(sid) => imp::reap_session(sid, &mut report),
        None => report.note(format!("child pid {child_pid} is not signalable")),
    }
    report
}

/// Holder side, when the pane ends: on the Linux scope route, end every other
/// process in the holder's own transient scope cgroup. A no-op elsewhere.
#[cfg(unix)]
pub fn reap_own_scope() -> ReapReport {
    let mut report = ReapReport::default();
    #[cfg(target_os = "linux")]
    if let Some(cgroup) = own_holder_scope_cgroup() {
        linux::reap_cgroup(&cgroup, &mut report);
    }
    report
}

impl ReapReport {
    /// Fold `other` into this report.
    pub fn absorb(&mut self, other: ReapReport) {
        self.killed.extend(other.killed);
        self.unattributed.extend(other.unattributed);
        self.notes.extend(other.notes);
    }
}

/// Sweep side, for a holder whose lock was found FREE (it is dead): end the
/// tree its record proves is the pane's. See the module docs, moment 2. The
/// caller holds that pane's lock, so no holder for it can start meanwhile.
pub fn reap_orphans_of_dead_holder(record: &LockRecord) -> ReapReport {
    let mut report = ReapReport::default();
    // The holder recorded a NORMAL end: what the child left running is its
    // user's, now and at every later sweep (plan D8 as decided 2026-10-14).
    if record.ended.as_deref() == Some(crate::lock::ENDED_BY_EXIT) {
        report.note("the pane ended by its child's own exit: its leftovers are not touched");
        return report;
    }
    #[cfg(target_os = "linux")]
    if let Some(unit) = &record.scope_unit {
        if unit.starts_with(crate::spawn::HOLDER_UNIT_PREFIX)
            && unit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            match crate::spawn::stop_scope_unit(unit, crate::spawn::SYSTEMCTL_DEADLINE) {
                // `--no-block`: success means the stop job was QUEUED.
                Ok(()) => report.note(format!("stop queued for scope unit {unit}")),
                Err(e) => report.note(format!("scope unit {unit} NOT stopped: {e}")),
            }
        } else {
            report.note(format!("refused a scope unit name not ours: {unit:?}"));
        }
    }
    match record.child_pid {
        None => report.note("record names no child"),
        Some(child) => imp::reap_recorded_child(child, record.child_start.as_deref(), &mut report),
    }
    report
}

#[cfg(target_os = "linux")]
pub use linux::own_holder_scope_cgroup;

#[cfg(target_os = "linux")]
mod linux {
    use super::ReapReport;
    use crate::pidfd::{self, Opened};
    use crate::proc_identity::{list_pids, stat};

    /// SIGKILL `pid` only if `still_ours(pid)` holds AFTER a handle on it was
    /// taken. `Some(detail)` names the route when a kill was delivered.
    pub(super) fn kill_verified(
        pid: i32,
        still_ours: impl Fn(i32) -> bool,
    ) -> Option<&'static str> {
        match pidfd::open(pid) {
            Opened::Gone(_) => None,
            Opened::Fd(fd) => {
                if !still_ours(pid) {
                    return None;
                }
                fd.send_signal(libc::SIGKILL).is_ok().then_some("pidfd")
            }
            Opened::NoPidFd => {
                if !still_ours(pid) {
                    return None;
                }
                // SAFETY: a plain signal to a pid in 2..=i32::MAX just verified.
                (unsafe { libc::kill(pid, libc::SIGKILL) } == 0).then_some("kill(pid), no pidfd")
            }
        }
    }

    /// Live (non-zombie) members of session `sid`, other than this process.
    pub(super) fn session_members(sid: i32) -> Vec<i32> {
        let me = std::process::id();
        list_pids()
            .into_iter()
            .filter(|&p| p != me)
            .filter_map(|p| {
                let s = stat(p)?;
                (s.session == sid && !s.zombie).then_some(p as i32)
            })
            .collect()
    }

    fn in_session(pid: i32, sid: i32) -> bool {
        stat(pid as u32).is_some_and(|s| s.session == sid && !s.zombie)
    }

    /// End every member of session `sid`, pass by pass.
    pub(super) fn reap_session(sid: i32, report: &mut ReapReport) {
        let mut routes = std::collections::BTreeSet::new();
        for _ in 0..super::REAP_PASSES {
            let members: Vec<i32> = session_members(sid)
                .into_iter()
                .filter(|p| !report.killed.contains(&(*p as u32)))
                .collect();
            if members.is_empty() {
                break;
            }
            for pid in members {
                if let Some(route) = kill_verified(pid, |p| in_session(p, sid)) {
                    report.killed.push(pid as u32);
                    routes.insert(route);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !routes.is_empty() {
            report.note(format!(
                "session {sid}: SIGKILL via {}",
                routes.into_iter().collect::<Vec<_>>().join("+")
            ));
        }
    }

    /// The cgroup v2 path of `pid` (`0::<path>`), if any.
    fn cgroup_v2_path(pid: &str) -> Option<String> {
        let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
        raw.lines()
            .find_map(|l| l.strip_prefix("0::"))
            .map(str::to_string)
    }

    /// This process's cgroup when it is a transient scope THIS crate's spawner
    /// made (`qontinui-pty-holder-*.scope`) — the only cgroup whose every
    /// member is provably the pane's. `None` on the plain route.
    pub fn own_holder_scope_cgroup() -> Option<String> {
        let path = cgroup_v2_path("self")?;
        let unit = path.rsplit('/').next()?;
        (unit.starts_with(crate::spawn::HOLDER_UNIT_PREFIX) && unit.ends_with(".scope"))
            .then_some(path)
    }

    /// End every process in cgroup `path` but this one.
    pub(super) fn reap_cgroup(path: &str, report: &mut ReapReport) {
        let procs = format!("/sys/fs/cgroup{path}/cgroup.procs");
        let me = std::process::id() as i32;
        let mut delivered = false;
        for _ in 0..super::REAP_PASSES {
            let Ok(raw) = std::fs::read_to_string(&procs) else {
                report.note(format!("cgroup {path}: cgroup.procs unreadable"));
                return;
            };
            let members: Vec<i32> = raw
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .filter(|&p| p != me && !report.killed.contains(&(p as u32)))
                .collect();
            if members.is_empty() {
                break;
            }
            for pid in members {
                let ours = |p: i32| cgroup_v2_path(&p.to_string()).as_deref() == Some(path);
                if kill_verified(pid, ours).is_some() {
                    report.killed.push(pid as u32);
                    delivered = true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if delivered {
            report.note(format!("scope cgroup {path}: SIGKILL"));
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{linux, ReapReport};
    use crate::proc_identity::start_token;

    pub(super) fn reap_session(sid: i32, report: &mut ReapReport) {
        linux::reap_session(sid, report);
    }

    pub(super) fn reap_recorded_child(child: u32, token: Option<&str>, report: &mut ReapReport) {
        let Some(sid) = crate::spawn::signalable_pid(child) else {
            report.note(format!("child pid {child} is not signalable"));
            return;
        };
        let Some(token) = token else {
            report.unattributed = linux::session_members(sid)
                .into_iter()
                .map(|p| p as u32)
                .collect();
            report.note(format!(
                "record has no child start token: {} process(es) in session {sid} are NOT \
                 provably the pane's (the id may since name another session) — left alone",
                report.unattributed.len()
            ));
            return;
        };
        // Leader verified alive → its session is the pane's. The leader's own
        // kill goes through the same verified path as every member.
        if start_token(child).as_deref() == Some(token) {
            report.note(format!("recorded child {child} is alive and verified"));
            linux::reap_session(sid, report);
        } else {
            report.unattributed = linux::session_members(sid)
                .into_iter()
                .map(|p| p as u32)
                .collect();
            report.note(format!(
                "recorded child {child} is gone: {} process(es) in session {sid} are NOT \
                 provably the pane's (the id may since name another session) — left alone",
                report.unattributed.len()
            ));
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod imp {
    use super::ReapReport;

    /// `killpg`: the process group the child leads (portable-pty `setsid()`s
    /// it, so its pgid is its pid). POSIX keeps that id from being reused
    /// while the group exists, so this names only the pane's group.
    pub(super) fn reap_session(sid: i32, report: &mut ReapReport) {
        // SAFETY: a plain signal to a process group id in 2..=i32::MAX.
        if unsafe { libc::killpg(sid, libc::SIGKILL) } == 0 {
            report.note(format!("process group {sid}: SIGKILL via killpg"));
        }
    }

    pub(super) fn reap_recorded_child(child: u32, _token: Option<&str>, report: &mut ReapReport) {
        report.note(format!(
            "no process start token on this OS: recorded child {child} is not verifiable and is \
             not signalled"
        ));
    }
}

#[cfg(windows)]
mod imp {
    use super::ReapReport;
    use qontinui_runner_win32::process_tree::{
        snapshot, terminate_tree_of, OwnedProcess, TreeKillError,
    };

    /// Wall-clock cap on ending one dead holder's tree. The walk itself
    /// (`qontinui_runner_win32::process_tree`) is shared with the runner's
    /// pane close, so the boot sweep and a tab close kill a tree the same way.
    const TREE_KILL_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    /// Every process the snapshot records as a child of `parent` pid, as
    /// UNATTRIBUTED: the parent is gone or unverifiable, so the link proves
    /// nothing — but the processes must not drop out of the orphan ledger.
    fn unattributed_children_of(parent: u32, report: &mut ReapReport) {
        let procs = match snapshot() {
            Ok(procs) => procs,
            Err(e) => {
                report.snapshot_unreadable = true;
                report.note(format!(
                    "process snapshot unreadable ({e}): the processes whose recorded parent \
                     is pid {parent} are UNKNOWN, not none"
                ));
                return;
            }
        };
        report.unattributed = procs
            .into_iter()
            .filter(|&(pid, ppid)| ppid == parent && pid != parent)
            .map(|(pid, _)| pid)
            .collect();
    }

    /// The recorded child, if alive and verified, plus every descendant —
    /// each joined to the tree only if created no earlier than its parent and
    /// no later than the snapshot, verified through a handle opened before the
    /// check and terminated through it, descendants first.
    pub(super) fn reap_recorded_child(child: u32, token: Option<&str>, report: &mut ReapReport) {
        let Some(token) = token else {
            unattributed_children_of(child, report);
            report.note(format!(
                "record has no child start token: child {child} is not verifiable; {} \
                 process(es) whose recorded parent is pid {child} are NOT provably its \
                 (the pid may since name another process) — left alone",
                report.unattributed.len()
            ));
            return;
        };
        let Ok(root) = OwnedProcess::open(child) else {
            unattributed_children_of(child, report);
            report.note(format!(
                "recorded child {child} is gone; {} process(es) whose recorded parent is pid \
                 {child} are NOT provably its (the pid may since name another process) — \
                 left alone",
                report.unattributed.len()
            ));
            return;
        };
        let Some(root_created) = root.creation_time() else {
            report.note(format!("recorded child {child}: creation time unreadable"));
            return;
        };
        if format!("windows:{root_created}") != token {
            report.note(format!(
                "pid {child} is a different process than the recorded child (reused pid)"
            ));
            return;
        }
        report.note(format!(
            "recorded child {child} is alive and verified (it was never contained by the \
             holder's job)"
        ));
        match terminate_tree_of(&root, child, TREE_KILL_BUDGET) {
            Ok(tree) => {
                if tree.budget_spent {
                    report.note(format!(
                        "tree kill of {child} hit its {TREE_KILL_BUDGET:?} budget after {} pass(es)",
                        tree.passes
                    ));
                }
                if tree.recheck_snapshot_failed {
                    // A child forked during the kills may have been missed:
                    // the tree's remainder is UNKNOWN, so keep the record.
                    report.snapshot_unreadable = true;
                    report.note(format!(
                        "tree kill of {child}: a re-check snapshot was unreadable"
                    ));
                }
                report.killed.extend(tree.killed);
            }
            Err(e) => {
                match &e {
                    TreeKillError::SnapshotFailed { killed, .. } => {
                        // Only the root was signalled; its descendants are
                        // UNKNOWN, so keep the record for the next sweep.
                        report.snapshot_unreadable = true;
                        report.killed.extend(killed.iter().copied());
                    }
                    TreeKillError::RootSurvived { killed, .. } => {
                        // The descendants that did die are still reported.
                        report.killed.extend(killed.iter().copied());
                    }
                    TreeKillError::Root(_) => {}
                }
                report.note(format!("tree kill of {child} failed: {e}"));
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    fn alive(pid: u32) -> bool {
        crate::proc_identity::stat(pid).is_some_and(|s| !s.zombie)
    }

    /// A stand-in pane: `sh` leads its own session and leaves a SIGHUP-immune
    /// background `sleep` in it, then waits.
    fn session_with_a_background_job() -> (std::process::Child, u32) {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "trap '' HUP; sleep 300 & echo $!; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        // SAFETY: setsid in the forked child before exec; async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut leader = cmd.spawn().unwrap();
        let mut line = String::new();
        use std::io::BufRead;
        std::io::BufReader::new(leader.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        (leader, line.trim().parse().unwrap())
    }

    /// Ends the fixture's whole session on drop, so a failed assert leaves no
    /// `sleep 300` behind. Only while ARMED: the session id is pinned to this
    /// fixture only while a member lives, so a test disarms it once it has
    /// seen every member dead — a later sweep could hit a recycled id.
    struct SessionGuard(u32, bool);

    impl SessionGuard {
        fn new(sid: u32) -> Self {
            SessionGuard(sid, true)
        }

        fn disarm(&mut self) {
            self.1 = false;
        }
    }

    impl Drop for SessionGuard {
        fn drop(&mut self) {
            if self.1 {
                let _ = reap_session_of_exited_child(self.0);
            }
        }
    }

    fn wait_dead(pid: u32) {
        let end = Instant::now() + Duration::from_secs(5);
        while alive(pid) {
            assert!(Instant::now() < end, "{pid} survived the reap");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn record(child: u32, token: Option<String>) -> LockRecord {
        LockRecord {
            holder_pid: 999_999,
            child_pid: Some(child),
            versions: vec![3],
            started_at_unix_ms: 0,
            holder_build: "test".into(),
            child_start: token,
            scope_unit: None,
            holder_start: None,
            ended: None,
        }
    }

    /// Moment 1: the pane's session remainder is ended; nothing outside it is.
    #[test]
    fn pty_holder_reap_session_of_exited_child_ends_the_session_and_nothing_else() {
        let (mut leader, bg) = session_with_a_background_job();
        let mut guard = SessionGuard::new(leader.id());
        let mut bystander = Command::new("sleep").arg("300").spawn().unwrap();
        let report = reap_session_of_exited_child(leader.id());
        assert!(report.killed.contains(&bg), "{report:?}");
        assert!(report.killed.contains(&leader.id()), "{report:?}");
        let _ = leader.wait();
        wait_dead(bg);
        guard.disarm();
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "a process outside the session was signalled"
        );
        let _ = bystander.kill();
        let _ = bystander.wait();
    }

    /// Moment 2, verified: the recorded child is alive and its token matches.
    #[test]
    fn pty_holder_reap_dead_holder_with_a_verified_child_ends_its_tree() {
        let (mut leader, bg) = session_with_a_background_job();
        let mut guard = SessionGuard::new(leader.id());
        let token = crate::proc_identity::start_token(leader.id());
        assert!(token.is_some());
        let report = reap_orphans_of_dead_holder(&record(leader.id(), token));
        assert!(report.killed.contains(&leader.id()), "{report:?}");
        let _ = leader.wait();
        wait_dead(bg);
        guard.disarm();
    }

    /// Moment 2, unverified: a token that does not match (the pid was reused,
    /// or the record is from another boot) signals NOTHING.
    #[test]
    fn pty_holder_reap_dead_holder_never_signals_an_unverified_pid() {
        let (mut leader, bg) = session_with_a_background_job();
        let mut guard = SessionGuard::new(leader.id());
        for token in [Some("linux:another-boot:1".to_string()), None] {
            let report = reap_orphans_of_dead_holder(&record(leader.id(), token));
            assert!(report.killed.is_empty(), "{report:?}");
            assert!(report.unattributed.contains(&bg), "{report:?}");
            assert!(leader.try_wait().unwrap().is_none());
            assert!(alive(bg));
        }
        let report = reap_session_of_exited_child(leader.id());
        assert!(report.killed.contains(&bg));
        let _ = leader.wait();
        wait_dead(bg);
        guard.disarm();
    }
}

/// The Windows boot-sweep reap, through the walk it now shares with the
/// runner's pane close: a recorded child whose start token does not match is
/// left alone (a reused pid), and one whose token matches is ended WITH its
/// tree (the `ping` below the recorded `cmd`).
#[cfg(all(test, windows))]
mod windows_reap_tests {
    use super::*;
    use qontinui_runner_win32::process_tree::{snapshot, OwnedProcess};
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn pty_holder_reap_windows_ends_a_verified_tree_and_spares_a_wrong_token() {
        // Every fixture process is in a kill-on-close job, so a failed
        // assertion leaves nothing running.
        let job = qontinui_runner_win32::ScopedKillOnCloseJob::create(None).expect("job");
        let mut child = Command::new("cmd")
            .args(["/c", "ping", "-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");
        // SAFETY: `child` is alive and owns its handle for this call.
        unsafe { job.assign(child.as_raw_handle() as _) };
        let pid = child.id();

        let end = Instant::now() + Duration::from_secs(20);
        let grandchild: OwnedProcess = loop {
            let found = snapshot()
                .expect("snapshot")
                .into_iter()
                .find(|&(p, pp)| pp == pid && p != pid)
                .and_then(|(p, _)| OwnedProcess::open(p).ok());
            if let Some(g) = found {
                break g;
            }
            assert!(Instant::now() < end, "cmd never started ping");
            std::thread::sleep(Duration::from_millis(50));
        };

        let mut wrong = ReapReport::default();
        imp::reap_recorded_child(pid, Some("windows:1"), &mut wrong);
        assert!(wrong.killed.is_empty(), "{wrong:?}");
        assert!(
            matches!(child.try_wait(), Ok(None)) && !grandchild.has_exited(),
            "a wrong token must not signal anything"
        );

        let token = crate::proc_identity::start_token(pid).expect("token");
        let mut right = ReapReport::default();
        imp::reap_recorded_child(pid, Some(&token), &mut right);
        assert!(right.killed.contains(&pid), "{right:?}");
        let end = Instant::now() + Duration::from_secs(5);
        while !(matches!(child.try_wait(), Ok(Some(_))) && grandchild.has_exited()) {
            assert!(
                Instant::now() < end,
                "the verified tree survived: {right:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(job);
    }
}
