//! Reaping a pane's process tree — the job the runner's `KILL_ON_JOB_CLOSE`
//! Job Object did for an in-process PTY, moved into the holder (plan D8).
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3. An out-of-process pane reports `pid() == None` to the runner, which
//! silently disables the runner's Job Object assignment; so crash-safety
//! reaping is the HOLDER's job, and under D13 each holder reaps exactly its own
//! child tree. Two moments, two entry points:
//!
//! 1. **The child exits, then the pane ends.** Whatever the child left running
//!    is ended by its holder, in two steps:
//!    - [`reap_session_of_exited_child`] — called by the holder's waiter the
//!      moment the child has exited and BEFORE it reaps it. The child is then a
//!      zombie, so its pid — which is its session and process-group id (it
//!      `setsid()`s onto its PTY) — cannot be reused by anyone: "session ==
//!      child pid" provably names the pane's session. Linux: every member of
//!      that session; other Unix: `killpg` of the child's group. Done later
//!      (after the reap), an emptied session's id could already belong to a
//!      stranger that called `setsid()`.
//!    - [`reap_own_scope`] — Linux scope route, when the pane ends, still under
//!      the pane lock: every other process in the holder's own transient
//!      scope cgroup, which also catches a descendant that `setsid()`ed out of
//!      the session. The cgroup is the holder's own, so it names nothing else.
//!    - Windows: the holder-owned job (`crate::pty`) is terminated, which ends
//!      every process in it; this module is not involved.
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
    #[cfg(target_os = "linux")]
    if let Some(unit) = &record.scope_unit {
        if unit.starts_with(crate::spawn::HOLDER_UNIT_PREFIX)
            && unit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            crate::spawn::stop_scope_unit(unit, crate::spawn::SYSTEMCTL_DEADLINE);
            report.note(format!("stopped scope unit {unit}"));
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
    use crate::proc_identity::{list_pids, stat};

    /// A pidfd, closed on drop.
    struct PidFd(libc::c_int);

    impl Drop for PidFd {
        fn drop(&mut self) {
            // SAFETY: a descriptor pidfd_open returned, closed exactly once.
            unsafe {
                libc::close(self.0);
            }
        }
    }

    enum Opened {
        Fd(PidFd),
        /// `pidfd_open` does not exist on this kernel.
        NoPidFd,
        /// The process is gone (or not ours to open).
        Gone,
    }

    fn pidfd_open(pid: i32) -> Opened {
        // SAFETY: pidfd_open(pid, 0) takes no pointers; the result is -1 or a
        // new descriptor owned by the returned PidFd.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd >= 0 {
            return Opened::Fd(PidFd(fd as libc::c_int));
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOSYS) {
            Opened::NoPidFd
        } else {
            Opened::Gone
        }
    }

    /// SIGKILL `pid` only if `still_ours(pid)` holds AFTER a handle on it was
    /// taken. `Some(detail)` names the route when a kill was delivered.
    pub(super) fn kill_verified(
        pid: i32,
        still_ours: impl Fn(i32) -> bool,
    ) -> Option<&'static str> {
        match pidfd_open(pid) {
            Opened::Gone => None,
            Opened::Fd(fd) => {
                if !still_ours(pid) {
                    return None;
                }
                // SAFETY: a pidfd we own; null siginfo, no flags.
                let rc = unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        fd.0,
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                };
                (rc == 0).then_some("pidfd")
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
            report.note("record has no child start token: nothing is verifiable");
            report.unattributed = linux::session_members(sid)
                .into_iter()
                .map(|p| p as u32)
                .collect();
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
                "recorded child {child} is gone; its session's remainder is not provably the pane's"
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
    use crate::proc_identity::creation_time;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    /// An owned process handle.
    struct Proc(HANDLE);

    impl Drop for Proc {
        fn drop(&mut self) {
            // SAFETY: a handle OpenProcess returned, closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn open(pid: u32) -> Option<Proc> {
        // SAFETY: no pointers; a non-null result is owned by `Proc`.
        let h = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        (!h.is_null()).then_some(Proc(h))
    }

    /// `(pid, parent pid)` of every process, from one Toolhelp snapshot.
    fn snapshot() -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        // SAFETY: a plain snapshot call; the handle is closed below.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return out;
        }
        // SAFETY: PROCESSENTRY32W is plain data; dwSize is set as required.
        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: a valid snapshot handle and a correctly sized entry.
        let mut more = unsafe { Process32FirstW(snap, &mut entry) } != 0;
        while more {
            out.push((entry.th32ProcessID, entry.th32ParentProcessID));
            // SAFETY: as above.
            more = unsafe { Process32NextW(snap, &mut entry) } != 0;
        }
        // SAFETY: closing the snapshot handle exactly once.
        unsafe {
            CloseHandle(snap);
        }
        out
    }

    /// The recorded child, if alive and verified, plus every descendant: a
    /// process joins the tree only if its parent is in the tree AND it was
    /// created no earlier than that parent (Windows keeps a dead parent's pid
    /// in `th32ParentProcessID`, and that pid may since have been reused by a
    /// process created later than the child it supposedly parents). Each is
    /// verified through a handle opened before the check, then terminated
    /// through that handle — descendants first.
    pub(super) fn reap_recorded_child(child: u32, token: Option<&str>, report: &mut ReapReport) {
        let Some(token) = token else {
            report.note(format!(
                "record has no child start token: child {child} is not verifiable"
            ));
            return;
        };
        let Some(root) = open(child) else {
            report.note(format!(
                "recorded child {child} is gone (the holder's job ended its tree)"
            ));
            return;
        };
        // SAFETY: `root` was opened with PROCESS_QUERY_LIMITED_INFORMATION.
        let Some(root_created) = (unsafe { creation_time(root.0) }) else {
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
        let procs = snapshot();
        let mut tree: Vec<(u32, u64, Proc)> = vec![(child, root_created, root)];
        let mut i = 0;
        while i < tree.len() {
            let (parent, parent_created) = (tree[i].0, tree[i].1);
            for &(pid, ppid) in &procs {
                if ppid != parent || pid == parent || tree.iter().any(|t| t.0 == pid) {
                    continue;
                }
                let Some(p) = open(pid) else { continue };
                // SAFETY: `p` was opened with PROCESS_QUERY_LIMITED_INFORMATION.
                match unsafe { creation_time(p.0) } {
                    Some(created) if created >= parent_created => tree.push((pid, created, p)),
                    _ => {}
                }
            }
            i += 1;
        }
        for (pid, _, p) in tree.iter().rev() {
            // SAFETY: a handle opened with PROCESS_TERMINATE and verified above.
            if unsafe { TerminateProcess(p.0, 1) } != 0 {
                report.killed.push(*pid);
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
        }
    }

    /// Moment 1: the pane's session remainder is ended; nothing outside it is.
    #[test]
    fn pty_holder_reap_session_of_exited_child_ends_the_session_and_nothing_else() {
        let (mut leader, bg) = session_with_a_background_job();
        let mut bystander = Command::new("sleep").arg("300").spawn().unwrap();
        let report = reap_session_of_exited_child(leader.id());
        assert!(report.killed.contains(&bg), "{report:?}");
        assert!(report.killed.contains(&leader.id()), "{report:?}");
        let _ = leader.wait();
        wait_dead(bg);
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
        let token = crate::proc_identity::start_token(leader.id());
        assert!(token.is_some());
        let report = reap_orphans_of_dead_holder(&record(leader.id(), token));
        assert!(report.killed.contains(&leader.id()), "{report:?}");
        let _ = leader.wait();
        wait_dead(bg);
    }

    /// Moment 2, unverified: a token that does not match (the pid was reused,
    /// or the record is from another boot) signals NOTHING.
    #[test]
    fn pty_holder_reap_dead_holder_never_signals_an_unverified_pid() {
        let (mut leader, bg) = session_with_a_background_job();
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
    }
}
