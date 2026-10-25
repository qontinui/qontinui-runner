//! Kill a direct child of this process AND every process it spawned, on Linux,
//! without ever signalling a stranger: the FREEZE WALK.
//!
//! Plan `2026-10-25-runner-kill-paths-never-kill-anything-off-windows`, Phase 1
//! (D-1). Until this module the runner's stop and shutdown paths killed a
//! process tree only on Windows; off Windows they killed nothing. A Unix pid is
//! a name, recycled once its process is reaped, and Linux re-parents an exited
//! process's children, so before signalling anything a kill must answer two
//! questions: *is this pid still the process we meant?* and *how do we reach
//! every descendant without signalling a stranger?*
//!
//! **The walk, as built** ([`kill_tree`]):
//!
//! 1. **Range and self.** `spawn::signalable_pid` refuses pid ≤ 1 and pids
//!    above `i32::MAX`; this process's own pid is refused ("refusing").
//! 2. **Pin and prove the root.** Take a pidfd on it ([`crate::pidfd`]), read
//!    its `/proc` stat, run `waitid(P_PID, root, WEXITED|WNOHANG|WNOWAIT)`, then
//!    `send_signal(fd, 0)`. The last call succeeding means the process behind
//!    the descriptor had not been reaped, so the stat and the `waitid` before it
//!    were about that process and no other.
//!    - `ECHILD`: the pid is not an unreaped child of this process — nothing is
//!      signalled.
//!    - The root is a zombie: `Ok` with `root_exited` and `passes == 0`, no
//!      member opened and no walk. A zombie pins nothing: its waiter may reap
//!      it at any moment, and its children are already re-parented.
//!    - An `expected_token` (the start token recorded when a tracked pid was
//!      spawned, `crate::proc_identity::start_token`'s format) that differs
//!      from the token read inside that window: "reused", nothing signalled.
//!    - Only then `SIGSTOP`. A stopped live process cannot exit on its own, so
//!      its waiter cannot reap it during the walk.
//! 3. **[`LinuxFreezeTable`]**, a `process_tree::ProcessTable` driven by the
//!    SAME generic `process_tree::walk` the Windows kill uses. Its `open` takes
//!    a pidfd, re-reads the stat, and stops the process — but only when its
//!    parent is the root or a member this table has already stopped and whose
//!    pidfd says it is still alive (vet D3), so no process outside the tree is
//!    ever stopped. Its `terminate` only marks a member doomed: every kill is
//!    deferred until the walk returns.
//!
//!    Whether a member has exited is answered by its PIDFD alone, never by
//!    `/proc`: a process whose main thread called `pthread_exit` shows its
//!    leader as `Z` there while its other threads run on, and a `/proc` read
//!    can fail (`EMFILE`, `ENFILE`) for a live process. Reading either as
//!    "exited" would report a live root as already gone and kill nothing.
//! 4. **The finalizer** (every return path, and on drop): `SIGKILL` each doomed
//!    member through its pidfd, `SIGCONT` each member this walk stopped and did
//!    not doom, and rebuild `report.killed` from the kills actually delivered.
//!
//! **Why this is sound.** The walk opens a parent before its children, and
//! every member is stopped as it is opened: `open` sends `SIGSTOP` and then
//! waits, for at most `STOP_CONFIRM` (200 ms), until every thread of the
//! process is in state `T`. A stopped member cannot exit on its own and a
//! stopped parent cannot reap, so in the ordinary case every member's pid stays
//! reserved until the walk returns. That alone is NOT what keeps strangers
//! out: a member that someone else `SIGKILL`s under a parent that ignores
//! `SIGCHLD` (or sets `SA_NOCLDWAIT`) is reaped by the kernel at once, and the
//! root's waiter is the runner itself, so a pid can still be freed mid-walk.
//! What keeps strangers out is the order inside `open`: the pidfd is taken
//! first, the stat read second, the parent is then required to be a member
//! whose pidfd says it is alive — alive now, so alive at the stat read, so the
//! read's ppid named that member — and the `SIGSTOP` through the new pidfd
//! succeeding proves the stat read was about the process behind it. Each
//! snapshot also drops the rows whose parent is a member whose pidfd says it
//! has exited. A child forked in the instant before its parent stopped is
//! visible in `/proc` by then, and the next pass finds it.
//!
//! **Known residue.** Start times tick at 10 ms: a pid reused within one tick
//! of a snapshot passes `admits_descendant`, but then needs a reserved parent
//! pid, which no stopped member's pid can be. Without `pidfd_open` (Linux <
//! 5.3) members are signalled by `kill(pid)` after a start-time re-check, as
//! `crate::terminate` does in its own fallback, and the log says so. A member
//! that refuses `SIGSTOP` (`EPERM`: a setuid descendant) is not walked, and the
//! log names it. A member whose threads do not all stop within
//! `STOP_CONFIRM` (one in uninterruptible I/O) can complete one more fork
//! before it stops; a later pass finds that child unless the walk has already
//! ended, and the log names the member. A process whose `/proc` entry or pidfd
//! cannot be read (an errno other than "no such process") is logged with its
//! errno and makes the report say `recheck_snapshot_failed`, so a walk that may
//! have missed a descendant never reports itself complete.

use std::cell::{Cell, RefCell};
use std::io;
use std::time::{Duration, Instant};

use qontinui_runner_win32::process_tree::{walk, ProcessTable, TreeKillError, TreeKillReport};

use crate::pidfd::{self, Opened, PidFd};
use crate::proc_identity;
use crate::spawn::signalable_pid;

/// Longest wait for one process's stop to take effect on every thread. A
/// sleeping thread stops at once; one in uninterruptible I/O stops when the
/// I/O completes. Past this the walk goes on, and the log says so.
const STOP_CONFIRM: Duration = Duration::from_millis(200);

/// Kill `pid` — a direct, unreaped child of this process — and every
/// descendant parent-linked to it, including ones that left its process group
/// or session. See the module docs for the order and the proofs.
///
/// `expected_token`, when given, must equal the pid's start token read inside
/// the pidfd window (`linux:<boot id>:<start ticks>`, the format
/// `proc_identity::start_token` answers); otherwise nothing is signalled.
/// `budget` bounds the walk between passes, as `process_tree::walk` does.
///
/// `Ok` with `root_exited` and `passes == 0` when the root was already a
/// zombie. `report.killed` lists the `SIGKILL`s actually delivered,
/// descendants first. `report.recheck_snapshot_failed` is also set when any
/// process's `/proc` entry or pidfd could not be read during the walk, since a
/// descendant may then have been missed. Every `Err` before the walk signalled
/// nothing.
pub fn kill_tree(
    pid: u32,
    expected_token: Option<&str>,
    budget: Duration,
) -> Result<TreeKillReport, TreeKillError> {
    kill_tree_with(pid, expected_token, budget, Handles::PidFd)
}

/// How members are signalled. Production always asks for a pidfd and falls
/// back per process only when the kernel has none; [`Handles::PidOnly`]
/// exercises that fallback where the kernel does have one (the tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handles {
    PidFd,
    PidOnly,
}

fn kill_tree_with(
    pid: u32,
    expected_token: Option<&str>,
    budget: Duration,
    handles: Handles,
) -> Result<TreeKillReport, TreeKillError> {
    match freeze_root(pid, expected_token, handles)? {
        Frozen::RootExited => Ok(TreeKillReport {
            root_exited: true,
            ..TreeKillReport::default()
        }),
        Frozen::Table(table) => {
            let walked = walk(&table, &ROOT, pid, std::process::id(), budget);
            table.finalize(walked)
        }
    }
}

/// The root's index in [`LinuxFreezeTable::members`].
const ROOT: usize = 0;

/// What step 2 left: a zombie root (nothing to walk), or a table holding the
/// stopped root.
enum Frozen {
    RootExited,
    Table(LinuxFreezeTable),
}

fn refuse(msg: String) -> TreeKillError {
    TreeKillError::Root(msg)
}

/// Steps 1 and 2 of the module docs.
fn freeze_root(
    pid: u32,
    expected_token: Option<&str>,
    handles: Handles,
) -> Result<Frozen, TreeKillError> {
    let Some(spid) = signalable_pid(pid) else {
        return Err(refuse(format!(
            "pid {pid}: refusing a pid outside 2..=i32::MAX; nothing signalled"
        )));
    };
    if pid == std::process::id() {
        return Err(refuse(format!(
            "pid {pid}: refusing to kill this process's own tree; nothing signalled"
        )));
    }
    let via = match open_handle(spid, handles) {
        Ok(via) => via,
        Err(e) if is_no_such_process(&e) => {
            return Err(refuse(format!(
                "pid {pid}: no such process ({e}: it has exited and been reaped, or never \
                 existed); nothing signalled"
            )))
        }
        Err(e) => {
            return Err(refuse(format!(
                "pid {pid}: pidfd_open failed ({e}); nothing signalled"
            )))
        }
    };
    if matches!(via, Via::Pid) && handles == Handles::PidFd {
        tracing::warn!(
            target: "tree_kill",
            "pid {pid}: pidfd_open is unavailable on this kernel; the tree is signalled by pid \
             after a start-time re-check of each member"
        );
    }
    // Read INSIDE the window: the descriptor is held, and the liveness check
    // below proves this read was about the process behind it.
    let st = match proc_identity::try_stat(pid) {
        Ok(Some(st)) => st,
        Ok(None) => {
            return Err(refuse(format!(
                "pid {pid}: no such process (its /proc entry is gone); nothing signalled"
            )))
        }
        Err(e) => {
            return Err(refuse(format!(
                "pid {pid}: /proc/{pid}/stat is unreadable ({e}); nothing signalled"
            )))
        }
    };
    let exited_pid = match waitid_nowait(spid) {
        Ok(p) => p,
        Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
            return Err(refuse(format!(
                "pid {pid} is not an unreaped child of this runner ({e}); nothing signalled"
            )))
        }
        Err(e) => {
            return Err(refuse(format!(
                "pid {pid}: waitid failed ({e}); nothing signalled"
            )))
        }
    };
    let mut root = Member {
        pid,
        start_ticks: st.start_ticks,
        via,
        resume: false,
        doomed: false,
    };
    if let Err(e) = root.signal(0) {
        return Err(refuse(format!(
            "pid {pid} was reaped while it was being checked ({e}); nothing signalled"
        )));
    }
    if exited_pid == spid {
        return Ok(Frozen::RootExited);
    }
    if let Some(want) = expected_token {
        match proc_identity::start_token_of(&st) {
            Some(have) if have == want => {}
            Some(have) => {
                return Err(refuse(format!(
                    "pid {pid} was reused: its start token is {have}, not the recorded \
                     {want}; nothing signalled"
                )))
            }
            None => {
                return Err(refuse(format!(
                    "pid {pid}: its start token is unreadable, so it cannot be matched to the \
                     recorded {want}; nothing signalled"
                )))
            }
        }
    }
    if let Err(e) = root.signal(libc::SIGSTOP) {
        return Err(refuse(format!(
            "pid {pid}: SIGSTOP refused ({e}); nothing signalled"
        )));
    }
    // A process already stopped by someone else (job control) stays stopped.
    root.resume = st.state != b'T';
    confirm_stopped(pid);
    Ok(Frozen::Table(LinuxFreezeTable {
        members: RefCell::new(vec![root]),
        doom_order: RefCell::new(Vec::new()),
        handles,
        incomplete: Cell::new(false),
        released: Cell::new(false),
    }))
}

/// `ESRCH` or `ENOENT`: the process is not there. Any other errno means the
/// question could not be answered.
fn is_no_such_process(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ESRCH)
}

/// `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)`: `Ok(pid)` when `pid` is an
/// exited (zombie) child, `Ok(0)` when it is a live child, `Err(ECHILD)` when it
/// is not an unreaped child of this process. Never reaps.
fn waitid_nowait(pid: i32) -> io::Result<i32> {
    loop {
        // SAFETY: an all-zero siginfo_t is a valid value (plain integers and
        // padding); WNOHANG leaves it zeroed when no child has exited.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, writable siginfo_t for the whole call.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            // SAFETY: waitid filled (or left zeroed) a SIGCHLD-shaped siginfo,
            // whose si_pid field is the one this reads.
            return Ok(unsafe { info.si_pid() });
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// How one member is signalled.
#[derive(Debug)]
enum Via {
    /// Through its pidfd: reaches that process or nothing.
    Fd(PidFd),
    /// By pid, after re-checking its start time (no pidfd on this kernel).
    Pid,
}

fn open_handle(pid: i32, handles: Handles) -> io::Result<Via> {
    if handles == Handles::PidOnly {
        return Ok(Via::Pid);
    }
    match pidfd::open(pid) {
        Opened::Fd(fd) => Ok(Via::Fd(fd)),
        Opened::NoPidFd => Ok(Via::Pid),
        Opened::Gone(e) => Err(e),
    }
}

/// One process this walk has stopped.
#[derive(Debug)]
struct Member {
    pid: u32,
    start_ticks: u64,
    via: Via,
    /// This walk stopped it (it was not already stopped), so it resumes it
    /// unless it is doomed.
    resume: bool,
    doomed: bool,
}

impl Member {
    /// Send `sig` to this member and to no other process.
    fn signal(&self, sig: libc::c_int) -> io::Result<()> {
        match &self.via {
            Via::Fd(fd) => fd.send_signal(sig),
            Via::Pid => {
                // Absent or another process: provably not this member (ESRCH).
                // Unreadable: its identity is UNKNOWN, so nothing is sent and
                // the read's own error is returned.
                match proc_identity::try_stat(self.pid)? {
                    Some(s) if s.start_ticks == self.start_ticks => {}
                    _ => return Err(io::Error::from_raw_os_error(libc::ESRCH)),
                }
                let pid = signalable_pid(self.pid)
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
                // SAFETY: a plain signal to a pid in 2..=i32::MAX whose start
                // time was re-checked just above (the no-pidfd residue).
                if unsafe { libc::kill(pid, sig) } == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            }
        }
    }

    /// Whether this member has exited (a zombie, or reaped).
    ///
    /// Through a pidfd, the pidfd is the ONLY authority (review round 1): a
    /// `/proc` stat shows a process whose main thread called `pthread_exit` as
    /// `Z` while its other threads run, and a stat read can fail for a live
    /// process. Without a pidfd, `/proc` is all there is: absent or a
    /// different start time is exited, a `Z` leader is exited only once no
    /// thread of it is left alive (a stopped thread is alive), and anything
    /// unreadable is UNKNOWN — answered "not exited", so the member is still
    /// killed, never dropped.
    fn exited(&self) -> bool {
        match &self.via {
            Via::Fd(fd) => fd.has_exited(),
            Via::Pid => match proc_identity::try_stat(self.pid) {
                Ok(None) => true,
                Ok(Some(s)) if s.start_ticks != self.start_ticks => true,
                Ok(Some(s)) if s.zombie => matches!(any_thread_alive(self.pid), Ok(false)),
                Ok(Some(_)) | Err(_) => false,
            },
        }
    }
}

/// The state letter of every thread of `pid` (`/proc/<pid>/task/*/stat`, read
/// as bytes). A thread that vanished mid-listing is left out; any other read
/// failure is an `Err`.
fn thread_states(pid: u32) -> io::Result<Vec<u8>> {
    let mut states = Vec::new();
    let threads = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(threads) => threads,
        Err(e) if is_no_such_process(&e) => return Ok(states),
        Err(e) => return Err(e),
    };
    for thread in threads {
        let path = thread?.path().join("stat");
        match std::fs::read(&path) {
            Ok(raw) => match proc_identity::parse_stat_bytes(&raw) {
                Some(st) => states.push(st.state),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{}: unparseable", path.display()),
                    ))
                }
            },
            Err(e) if is_no_such_process(&e) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(states)
}

/// Whether any thread of `pid` is neither stopped nor dead.
fn any_thread_running(pid: u32) -> io::Result<bool> {
    Ok(thread_states(pid)?
        .into_iter()
        .any(|s| !matches!(s, b'T' | b't' | b'Z' | b'X' | b'x')))
}

/// Whether any thread of `pid` has not exited — a STOPPED thread is alive.
/// What decides, without a pidfd, whether a process whose leader shows `Z` has
/// really exited.
fn any_thread_alive(pid: u32) -> io::Result<bool> {
    Ok(thread_states(pid)?
        .into_iter()
        .any(|s| !matches!(s, b'Z' | b'X' | b'x')))
}

/// Wait, at most [`STOP_CONFIRM`], until every thread of `pid` is stopped,
/// traced-stopped, dead, or gone. `false` when the wait ran out.
fn confirm_stopped(pid: u32) -> bool {
    let end = Instant::now() + STOP_CONFIRM;
    loop {
        if all_threads_stopped(pid) {
            return true;
        }
        if Instant::now() >= end {
            tracing::warn!(
                target: "tree_kill",
                "pid {pid}: SIGSTOP delivered but not every thread stopped within \
                 {STOP_CONFIRM:?}; a child it forks meanwhile is found only by a later pass"
            );
            return false;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

fn all_threads_stopped(pid: u32) -> bool {
    // A thread whose state cannot be read counts as running.
    matches!(any_thread_running(pid), Ok(false))
}

/// `CLOCK_BOOTTIME` in `sysconf(_SC_CLK_TCK)` ticks: the clock and unit of
/// `/proc/<pid>/stat` field 22, so start times compare against it directly.
fn boottime_ticks() -> u64 {
    // SAFETY: sysconf takes no pointers.
    let hz = match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } {
        hz if hz > 0 => hz as u64,
        _ => 100,
    };
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the call.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        // Unreadable: "now" is the epoch, so no process is admitted as created
        // before it and the walk reaches the root alone — toward less.
        return 0;
    }
    (ts.tv_sec as u64) * hz + (ts.tv_nsec as u64) * hz / 1_000_000_000
}

/// The Linux `process_tree::ProcessTable`: `/proc` for the snapshot, a pidfd
/// per member for every signal, and stop-on-open for the pid pinning the walk
/// relies on. A handle is an index into `members`; the table owns the
/// descriptors so that the finalizer can reach every member it stopped,
/// including one the walk opened and then declined.
pub struct LinuxFreezeTable {
    /// The root first, then every process `open` stopped, in opening order.
    members: RefCell<Vec<Member>>,
    /// Indices `terminate` marked, in the walk's order (descendants first).
    doom_order: RefCell<Vec<usize>>,
    handles: Handles,
    /// A process's `/proc` entry or pidfd could not be read during the walk
    /// (an errno other than "no such process"): a descendant may be missing.
    incomplete: Cell<bool>,
    /// The finalizer has run (it runs once: explicitly, or on drop).
    released: Cell<bool>,
}

impl ProcessTable for LinuxFreezeTable {
    type Handle = usize;

    fn now(&self) -> u64 {
        boottime_ticks()
    }

    fn snapshot(&self) -> io::Result<Vec<(u32, u32)>> {
        let pids = proc_identity::try_list_pids()?;
        // A member that has exited no longer pins its pid; a row naming it as
        // parent may be a stranger's child (vet D2).
        let dead_parents: Vec<u32> = self
            .members
            .borrow()
            .iter()
            .filter(|m| m.exited())
            .map(|m| m.pid)
            .collect();
        let mut rows = Vec::with_capacity(pids.len());
        let mut first_unreadable: Option<(u32, io::Error)> = None;
        let mut unreadable = 0usize;
        for pid in pids {
            match proc_identity::try_stat(pid) {
                Ok(Some(st)) => {
                    if let Ok(ppid) = u32::try_from(st.ppid) {
                        if !dead_parents.contains(&ppid) {
                            rows.push((pid, ppid));
                        }
                    }
                }
                // Exited since the listing: not a descendant to reach.
                Ok(None) => {}
                Err(e) => {
                    unreadable += 1;
                    first_unreadable.get_or_insert((pid, e));
                }
            }
        }
        if let Some((pid, e)) = first_unreadable {
            self.incomplete.set(true);
            tracing::warn!(
                target: "tree_kill",
                "process snapshot: {unreadable} /proc stat file(s) unreadable (first: pid {pid}, \
                 {e}); a descendant among them is not reached"
            );
        }
        Ok(rows)
    }

    fn open(&self, pid: u32) -> Option<usize> {
        // Already stopped by this walk (a child the walk declined on creation
        // time in an earlier pass is offered again): the same member.
        let known = self.members.borrow().iter().position(|m| m.pid == pid);
        if let Some(i) = known {
            return (!self.members.borrow()[i].exited()).then_some(i);
        }
        let via = match open_handle(signalable_pid(pid)?, self.handles) {
            Ok(via) => via,
            Err(e) if is_no_such_process(&e) => return None,
            Err(e) => {
                self.note_unreachable(pid, "pidfd_open failed", &e);
                return None;
            }
        };
        let st = match proc_identity::try_stat(pid) {
            Ok(Some(st)) => st,
            Ok(None) => return None,
            Err(e) => {
                self.note_unreachable(pid, "/proc stat unreadable", &e);
                return None;
            }
        };
        // Vet D3: stop ONLY a child of a process this walk already holds
        // stopped and that has not exited. Such a parent's pid is its own, so
        // this process is in the tree.
        let parent_in_tree = u32::try_from(st.ppid).is_ok_and(|ppid| {
            self.members
                .borrow()
                .iter()
                .any(|m| m.pid == ppid && !m.exited())
        });
        if !parent_in_tree {
            return None;
        }
        let member = Member {
            pid,
            start_ticks: st.start_ticks,
            via,
            resume: st.state != b'T',
            doomed: false,
        };
        // Exited — through the pidfd just taken, not the stat's `Z`, which a
        // live process whose main thread exited also shows: nothing to stop or
        // kill, and its children are already re-parented away from it.
        if member.exited() {
            return None;
        }
        // Success also proves the stat above was this process's: the signal
        // reached the process behind the handle, so it had not been reaped.
        if let Err(e) = member.signal(libc::SIGSTOP) {
            if !is_no_such_process(&e) {
                self.note_unreachable(pid, "SIGSTOP refused", &e);
            }
            return None;
        }
        confirm_stopped(pid);
        let mut members = self.members.borrow_mut();
        members.push(member);
        Some(members.len() - 1)
    }

    fn creation_time(&self, h: &usize) -> Option<u64> {
        self.members.borrow().get(*h).map(|m| m.start_ticks)
    }

    fn terminate(&self, h: &usize) -> bool {
        let mut members = self.members.borrow_mut();
        let Some(m) = members.get_mut(*h) else {
            return false;
        };
        if !m.doomed {
            m.doomed = true;
            self.doom_order.borrow_mut().push(*h);
        }
        true
    }

    fn has_exited(&self, h: &usize) -> bool {
        self.members.borrow().get(*h).is_none_or(Member::exited)
    }
}

impl LinuxFreezeTable {
    /// A process in (or possibly in) the tree that this walk could not reach:
    /// logged with its errno, and the report will not claim a complete walk.
    fn note_unreachable(&self, pid: u32, what: &str, e: &io::Error) {
        self.incomplete.set(true);
        tracing::warn!(
            target: "tree_kill",
            "pid {pid}: {what} ({e}); it and its subtree are not walked"
        );
    }

    /// Step 4: kill the doomed, resume the rest this walk stopped, and make the
    /// walk's answer say what was actually delivered.
    fn finalize(
        &self,
        walked: Result<TreeKillReport, TreeKillError>,
    ) -> Result<TreeKillReport, TreeKillError> {
        let killed = self.release();
        match walked {
            Ok(mut report) => {
                let members = self.members.borrow();
                let root = &members[ROOT];
                let root_pid = root.pid;
                if root.doomed && !killed.contains(&root_pid) {
                    if root.exited() {
                        report.root_exited = true;
                    } else {
                        return Err(TreeKillError::RootSurvived { root_pid, killed });
                    }
                }
                report.killed = killed;
                if self.incomplete.get() {
                    report.recheck_snapshot_failed = true;
                }
                Ok(report)
            }
            Err(TreeKillError::SnapshotFailed {
                root_pid, detail, ..
            }) => Err(TreeKillError::SnapshotFailed {
                root_pid,
                detail,
                killed,
            }),
            Err(TreeKillError::RootSurvived { root_pid, .. }) => {
                Err(TreeKillError::RootSurvived { root_pid, killed })
            }
            Err(e @ TreeKillError::Root(_)) => Err(e),
        }
    }

    /// The finalizer proper, once. Returns the pids a `SIGKILL` was delivered
    /// to, in the walk's order.
    fn release(&self) -> Vec<u32> {
        if self.released.replace(true) {
            return Vec::new();
        }
        let members = self.members.borrow();
        let mut killed = Vec::new();
        for &i in self.doom_order.borrow().iter() {
            let m = &members[i];
            match m.signal(libc::SIGKILL) {
                Ok(()) => killed.push(m.pid),
                // Reaped, or (no pidfd) the pid now names another process.
                Err(e) if is_no_such_process(&e) => {}
                Err(e) => {
                    // Not provably gone, and possibly stopped by this walk: do
                    // not leave it frozen (review round 1, finding 3).
                    tracing::warn!(
                        target: "tree_kill",
                        "pid {}: SIGKILL not delivered ({e}) and it is not provably gone",
                        m.pid
                    );
                    if m.resume {
                        if let Err(e) = m.signal(libc::SIGCONT) {
                            tracing::warn!(
                                target: "tree_kill",
                                "pid {}: SIGCONT not delivered either ({e}); it may be left \
                                 stopped",
                                m.pid
                            );
                        }
                    }
                }
            }
        }
        for m in members.iter().filter(|m| m.resume && !m.doomed) {
            if let Err(e) = m.signal(libc::SIGCONT) {
                tracing::warn!(
                    target: "tree_kill",
                    "pid {}: SIGCONT not delivered ({e}); it may be left stopped",
                    m.pid
                );
            }
        }
        killed
    }
}

impl Drop for LinuxFreezeTable {
    /// A walk that unwound before its finalizer still leaves nothing stopped.
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::proc_identity::ProcStat;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, ExitStatus, Stdio};

    const BUDGET: Duration = Duration::from_secs(5);

    fn stat(pid: u32) -> Option<ProcStat> {
        proc_identity::stat(pid)
    }

    /// Absent, a zombie, or a different process now.
    fn gone(pid: u32, start: u64) -> bool {
        stat(pid).is_none_or(|s| s.zombie || s.start_ticks != start)
    }

    fn state(pid: u32) -> Option<u8> {
        stat(pid).map(|s| s.state)
    }

    /// SIGKILL `(pid, start)` only while that pid is still that process: the
    /// fixtures' own cleanup, which can therefore never reach a stranger.
    fn kill_if_same(pid: u32, start: u64) {
        let Some(spid) = signalable_pid(pid) else {
            return;
        };
        if let Opened::Fd(fd) = pidfd::open(spid) {
            if stat(pid).is_some_and(|s| s.start_ticks == start) {
                let _ = fd.send_signal(libc::SIGKILL);
            }
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Every descendant of `root` reachable by parent links right now.
    fn descendants_of(root: u32) -> Vec<(u32, u64)> {
        let rows: Vec<(u32, ProcStat)> = proc_identity::try_list_pids()
            .unwrap()
            .into_iter()
            .filter_map(|p| Some((p, stat(p)?)))
            .collect();
        let mut out: Vec<(u32, u64)> = Vec::new();
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for (pid, st) in &rows {
                if st.ppid as u32 == parent && !out.iter().any(|(p, _)| p == pid) {
                    out.push((*pid, st.start_ticks));
                    frontier.push(*pid);
                }
            }
        }
        out
    }

    /// A process tree this test spawned. Dropping it kills what is left of it
    /// — the root through its `Child`, every recorded descendant only while
    /// its pid still names it — so a failed assertion leaves nothing running.
    struct Tree {
        child: Child,
        recorded: Vec<(u32, u64)>,
        /// Spawned as its own process group (`process_group(0)`).
        own_group: bool,
        /// The root has been waited on: its pid no longer pins the group id.
        reaped: bool,
    }

    impl Tree {
        fn sh(script: &str, own_group: bool) -> Tree {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", script]);
            Tree::spawn(cmd, own_group)
        }

        fn spawn(mut cmd: Command, own_group: bool) -> Tree {
            cmd.stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            if own_group {
                // Its own process group, so even a group kill (mutation M4)
                // can never reach the test harness.
                cmd.process_group(0);
            }
            Tree {
                child: cmd.spawn().expect("spawn the fixture"),
                recorded: Vec::new(),
                own_group,
                reaped: false,
            }
        }

        fn wait(&mut self) -> ExitStatus {
            let status = self.child.wait().unwrap();
            self.reaped = true;
            status
        }

        /// `Child::try_wait`, remembering a reap so `Drop` never group-kills
        /// an id the reaped root no longer pins.
        fn try_wait(&mut self) -> Option<ExitStatus> {
            let status = self.child.try_wait().unwrap();
            if status.is_some() {
                self.reaped = true;
            }
            status
        }

        fn pid(&self) -> u32 {
            self.child.id()
        }

        fn start(&self) -> u64 {
            stat(self.pid()).expect("root stat").start_ticks
        }

        /// Wait (bounded) for at least `n` descendants, recording them on
        /// every round, so a timeout still leaves them for `Drop` to kill.
        fn await_descendants(&mut self, n: usize) {
            let root = self.pid();
            let end = Instant::now() + Duration::from_secs(5);
            loop {
                self.recorded = descendants_of(root);
                if self.recorded.len() >= n {
                    return;
                }
                assert!(
                    Instant::now() < end,
                    "timed out waiting for {n} descendants of the fixture: {:?}",
                    self.recorded
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn first_line(&mut self) -> String {
            use std::io::BufRead;
            let mut line = String::new();
            std::io::BufReader::new(self.child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            line.trim().to_string()
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            for &(pid, start) in &self.recorded {
                kill_if_same(pid, start);
            }
            if self.own_group && !self.reaped {
                // The unreaped root pins its pid, so no other process can lead
                // a group with that id: every member is one this test spawned.
                // Repeated, to catch a fork that was in flight.
                if let Some(pgrp) = signalable_pid(self.pid()) {
                    for _ in 0..3 {
                        // SAFETY: a plain signal to a process group this
                        // fixture created and whose id its unreaped root pins.
                        unsafe { libc::killpg(pgrp, libc::SIGKILL) };
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Assert `(pid, start)` is gone within 5 s — absent, a zombie, or another
    /// process — which a member left stopped never is; a failure names the
    /// state it was left in.
    fn assert_ends(pid: u32, start: u64) {
        let end = Instant::now() + Duration::from_secs(5);
        while !gone(pid, start) {
            assert!(
                Instant::now() < end,
                "pid {pid} survived the tree kill (state {:?})",
                state(pid).map(char::from)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// T1: a real three-level tree ends, every member, root through `SIGKILL`.
    #[test]
    fn ends_every_process_of_a_real_three_level_tree() {
        let mut tree = Tree::sh("sleep 300 & sh -c 'sleep 300 & wait' & wait", false);
        tree.await_descendants(3);
        let root = tree.pid();
        let report = kill_tree(root, None, BUDGET).expect("tree kill");
        assert!(report.killed.contains(&root), "{report:?}");
        for (pid, _) in &tree.recorded {
            assert!(report.killed.contains(pid), "{pid} not killed: {report:?}");
        }
        let status = tree.wait();
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
        for &(pid, start) in &tree.recorded {
            assert_ends(pid, start);
        }
    }

    /// T2: a descendant that called `setsid()` — out of the root's process
    /// group AND session, where a group kill cannot reach — is still killed.
    #[test]
    fn reaches_a_descendant_that_left_the_group_and_session() {
        let has_setsid = Command::new("sh")
            .args(["-c", "command -v setsid"])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(has_setsid, "this test needs util-linux `setsid` on PATH");
        let mut tree = Tree::sh("sh -c 'setsid sleep 300 & wait' & wait", true);
        let root = tree.pid();
        let root_stat = stat(root).unwrap();
        let mut detached = None;
        wait_until("the setsid grandchild", || {
            detached = descendants_of(root).into_iter().find(|&(p, _)| {
                stat(p).is_some_and(|s| s.session == p as i32 && s.ppid as u32 != root)
            });
            detached.is_some()
        });
        let (gc, gc_start) = detached.unwrap();
        tree.recorded = descendants_of(root);
        let gc_stat = stat(gc).unwrap();
        assert_ne!(
            gc_stat.session, root_stat.session,
            "precondition: own session"
        );
        assert_ne!(gc_stat.pgrp, root_stat.pgrp, "precondition: own group");
        let report = kill_tree(root, None, BUDGET).expect("tree kill");
        assert!(report.killed.contains(&gc), "{report:?}");
        assert_ends(gc, gc_start);
    }

    /// T3: a grandchild — alive, but not a child of this process — is refused,
    /// and left alive and running.
    #[test]
    fn refuses_a_pid_that_is_not_this_processs_child() {
        let mut tree = Tree::sh("sleep 300 & echo $!; wait", false);
        let gc: u32 = tree.first_line().parse().expect("grandchild pid");
        tree.await_descendants(1);
        let gc_start = stat(gc).expect("grandchild stat").start_ticks;
        let err = kill_tree(gc, None, BUDGET).expect_err("a grandchild must be refused");
        assert!(err.to_string().contains(&gc.to_string()), "{err}");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!gone(gc, gc_start), "the grandchild was killed");
        assert_ne!(state(gc), Some(b'T'), "the grandchild was left stopped");
        assert!(tree.try_wait().is_none(), "the root was killed");
    }

    /// T4: a child that was already reaped signals nothing. This reaches the
    /// `pidfd_open` ESRCH arm. Residue: a reaped pid that ANOTHER child of this
    /// process has since been given passes the `waitid` proof; for a tracked
    /// pid the start token recorded at spawn (`expected_token`) closes that.
    #[test]
    fn signals_nothing_for_a_reaped_child() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let err = kill_tree(pid, None, BUDGET).expect_err("a reaped child must be refused");
        assert!(err.to_string().contains("no such process"), "{err}");
    }

    /// T5: this process is never a root.
    #[test]
    fn refuses_this_process() {
        let err = kill_tree(std::process::id(), None, BUDGET).expect_err("self must be refused");
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    /// T6: a member the walk stopped but never doomed is resumed by the
    /// finalizer — alive and running, not left in state `T`.
    #[test]
    fn finalizer_resumes_a_stopped_member_it_did_not_doom() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let (member, member_start) = tree.recorded[0];
        let Frozen::Table(table) =
            freeze_root(tree.pid(), None, Handles::PidFd).expect("freeze root")
        else {
            panic!("a live root froze as exited");
        };
        let h = table
            .open(member)
            .expect("a child of the frozen root opens");
        assert_eq!(table.creation_time(&h), Some(member_start));
        // `open` waits for the stop only up to STOP_CONFIRM: on a loaded box
        // give the state change its own bounded wait.
        wait_until("open to stop the member", || state(member) == Some(b'T'));
        let report = table
            .finalize(Ok(TreeKillReport::default()))
            .expect("finalize");
        assert!(report.killed.is_empty(), "{report:?}");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!gone(member, member_start), "an undoomed member was killed");
        assert_ne!(
            state(member),
            Some(b'T'),
            "an undoomed member was left stopped"
        );
        assert_ne!(state(tree.pid()), Some(b'T'), "the root was left stopped");
    }

    /// T6b: `open` refuses — and never stops — a process whose parent is not
    /// in the table (vet D3).
    #[test]
    fn open_never_stops_a_process_outside_the_tree() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let mut outsider = Tree::sh("exec sleep 300", false);
        let outsider_pid = outsider.pid();
        outsider.recorded = vec![(outsider_pid, outsider.start())];
        let Frozen::Table(table) =
            freeze_root(tree.pid(), None, Handles::PidFd).expect("freeze root")
        else {
            panic!("a live root froze as exited");
        };
        assert_eq!(table.open(outsider_pid), None);
        assert_ne!(state(outsider_pid), Some(b'T'), "an outsider was stopped");
        drop(table);
        assert!(outsider.try_wait().is_none(), "an outsider was killed");
        assert_ne!(
            state(outsider_pid),
            Some(b'T'),
            "an outsider was left stopped"
        );
    }

    /// T7: killing an already-killed (but unreaped) root again is harmless —
    /// reported exited, no walk, nothing signalled.
    #[test]
    fn killing_twice_is_harmless() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let root = tree.pid();
        let first = kill_tree(root, None, BUDGET).expect("first kill");
        assert!(first.killed.contains(&root), "{first:?}");
        wait_until("the killed root to become a zombie", || {
            stat(root).is_some_and(|s| s.zombie)
        });
        let second = kill_tree(root, None, BUDGET).expect("second kill");
        assert!(second.root_exited, "{second:?}");
        assert_eq!(second.passes, 0, "{second:?}");
        assert!(second.killed.is_empty(), "{second:?}");
        tree.wait();
    }

    /// The `sleep 300.4711` processes in group `pgrp`, started at or after
    /// `since`, that are still alive.
    fn live_marked_sleeps(pgrp: u32, since: u64) -> Vec<(u32, u64)> {
        proc_identity::try_list_pids()
            .unwrap()
            .into_iter()
            .filter_map(|p| {
                let s = stat(p)?;
                let ours = !s.zombie && s.pgrp == pgrp as i32 && s.start_ticks >= since;
                let cmdline = std::fs::read(format!("/proc/{p}/cmdline")).ok()?;
                let marked = cmdline.split(|&b| b == 0).any(|arg| arg == b"300.4711");
                (ours && marked).then_some((p, s.start_ticks))
            })
            .collect()
    }

    /// T11: a member that forks without pause leaves no child behind — each
    /// member is stopped as it is opened, so nothing forks after it is found.
    #[test]
    fn leaves_no_child_of_a_forking_member() {
        let since = boottime_ticks();
        let tree = Tree::sh(
            "sh -c 'while :; do sleep 300.4711 & sleep 0.005; done' & wait",
            true,
        );
        let root = tree.pid();
        wait_until("the forking member's first sleeps", || {
            live_marked_sleeps(root, since).len() >= 5
        });
        let report = kill_tree(root, None, BUDGET);
        let settled = Instant::now() + Duration::from_secs(5);
        let survivors = loop {
            let left = live_marked_sleeps(root, since);
            if left.is_empty() || Instant::now() >= settled {
                break left;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // They are this test's own (its group, its marker): clean them up
        // before asserting, so a red run leaves nothing for 300 s.
        for &(pid, start) in &survivors {
            kill_if_same(pid, start);
        }
        let report = report.expect("tree kill");
        assert!(report.killed.contains(&root), "{report:?}");
        assert!(
            survivors.is_empty(),
            "children outlived the kill: {survivors:?} ({report:?})"
        );
    }

    /// T12: a root that exited but was not waited on is reported exited, with
    /// no walk and nothing opened.
    #[test]
    fn zombie_root_is_reported_exited_without_a_walk() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        wait_until("the child to become a zombie", || {
            stat(pid).is_some_and(|s| s.zombie)
        });
        let report = kill_tree(pid, None, BUDGET).expect("a zombie root is Ok");
        assert!(report.root_exited, "{report:?}");
        assert_eq!(report.passes, 0, "{report:?}");
        assert!(report.killed.is_empty(), "{report:?}");
        child.wait().unwrap();
    }

    /// T13: a live child whose start token is not the recorded one is refused
    /// as reused — alive and running afterwards.
    #[test]
    fn wrong_expected_token_signals_nothing() {
        let mut tree = Tree::sh("exec sleep 300", false);
        let pid = tree.pid();
        let start = tree.start();
        let err = kill_tree(pid, Some("linux:not-this-boot:1"), BUDGET)
            .expect_err("a wrong token must be refused");
        assert!(err.to_string().contains("reused"), "{err}");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!gone(pid, start), "the child was killed");
        assert_ne!(state(pid), Some(b'T'), "the child was left stopped");
        assert!(tree.try_wait().is_none(), "the child was killed");
    }

    /// T13's other half: the token recorded at spawn is accepted, so the
    /// refusal above is about the token and not a refusal of every token.
    #[test]
    fn matching_expected_token_kills_the_tree() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let token = proc_identity::start_token(tree.pid()).expect("token");
        let report = kill_tree(tree.pid(), Some(&token), BUDGET).expect("tree kill");
        assert!(report.killed.contains(&tree.pid()), "{report:?}");
        let (pid, start) = tree.recorded[0];
        assert_ends(pid, start);
    }

    /// A root whose MAIN thread calls `pthread_exit` while another thread
    /// runs on, after starting a `sleep 300` child whose pid it prints.
    const LEADER_EXITS: &str = "import ctypes, subprocess, threading, time\n\
        child = subprocess.Popen(['sleep', '300'])\n\
        print(child.pid, flush=True)\n\
        threading.Thread(target=time.sleep, args=(300,)).start()\n\
        ctypes.CDLL(None).pthread_exit(None)\n";

    /// Review round 1, finding 1: `/proc/<pid>/stat` shows such a root's
    /// leader as `Z`, yet the process is alive — its pidfd is not readable and
    /// `waitid` does not report it. It and its child must be killed, never
    /// reported "already exited" with nothing killed.
    fn kills_a_root_whose_main_thread_exited(handles: Handles) {
        let has_ctypes = Command::new("python3")
            .args(["-c", "import ctypes, threading"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(has_ctypes, "this test needs python3 (with ctypes) on PATH");
        let mut cmd = Command::new("python3");
        cmd.args(["-c", LEADER_EXITS]);
        let mut tree = Tree::spawn(cmd, false);
        let child: u32 = tree.first_line().parse().expect("child pid");
        tree.recorded = vec![(child, stat(child).expect("child stat").start_ticks)];
        let root = tree.pid();
        wait_until(
            "the root's main thread to exit while another thread runs",
            || stat(root).is_some_and(|s| s.zombie) && matches!(any_thread_running(root), Ok(true)),
        );
        let report = kill_tree_with(root, None, BUDGET, handles).expect("tree kill");
        assert!(
            !report.root_exited,
            "a live root read as exited: {report:?}"
        );
        assert!(report.killed.contains(&root), "{report:?}");
        assert!(report.killed.contains(&child), "{report:?}");
        wait_until("the root to be reaped", || tree.try_wait().is_some());
        let (child, child_start) = tree.recorded[0];
        assert_ends(child, child_start);
    }

    #[test]
    fn kills_a_root_whose_main_thread_exited_through_its_pidfd() {
        kills_a_root_whose_main_thread_exited(Handles::PidFd);
    }

    #[test]
    fn kills_a_root_whose_main_thread_exited_without_pidfd() {
        kills_a_root_whose_main_thread_exited(Handles::PidOnly);
    }

    /// The no-pidfd fallback (Linux < 5.3), forced: members are signalled by
    /// pid after a start-time re-check, and the tree still ends.
    #[test]
    fn without_pidfd_the_tree_still_ends() {
        let mut tree = Tree::sh("sleep 300 & sh -c 'sleep 300 & wait' & wait", false);
        tree.await_descendants(3);
        let root = tree.pid();
        let report = kill_tree_with(root, None, BUDGET, Handles::PidOnly).expect("tree kill");
        assert!(report.killed.contains(&root), "{report:?}");
        for &(pid, start) in &tree.recorded {
            assert!(report.killed.contains(&pid), "{pid} not killed: {report:?}");
            assert_ends(pid, start);
        }
    }
}
