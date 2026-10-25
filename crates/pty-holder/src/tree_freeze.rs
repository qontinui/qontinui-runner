//! Kill a direct child of this process AND every process it spawned, on Linux,
//! without signalling a stranger — provided the caller has pinned WHICH child
//! it means: the FREEZE WALK.
//!
//! **The root's identity is the caller's to pin.** The walk proves the root is
//! an unreaped child of this process; it cannot prove it is the child the
//! caller meant. Pass the start token recorded at spawn (`expected_token`),
//! or pass `None` ONLY while the caller still holds that child's
//! `std::process::Child` / `tokio::process::Child` UNWAITED — no `wait` /
//! `try_wait` has returned its exit, AND none is in flight (a `tokio`
//! `Child::wait()` future being polled elsewhere reaps the moment the child
//! exits). Such a child is never reaped, so its pid cannot be reused. `None`
//! after the child was waited on, while a wait is in flight, or from a caller
//! that never held it is unsound: a pid
//! reaped and reused by ANOTHER child of this process passes every check, and
//! that child is killed with its whole tree. That residue is the caller's
//! contract, not something this module can close.
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
//!    - An `expected_token` (the start token recorded when a tracked pid was
//!      spawned, `crate::proc_identity::start_token`'s format) that differs
//!      from the token read inside that window: "reused", nothing signalled.
//!      Checked first, so a zombie with the wrong token is "reused" too.
//!    - The root is a zombie: `Ok` with `root_exited` and `passes == 0`, no
//!      member opened and no walk. A zombie pins nothing: its waiter may reap
//!      it at any moment, and its children are already re-parented.
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
//! 4. **The finalizer** (every return path, and on drop): after a walk, every
//!    member `open` stopped is doomed — including one the walk declined on
//!    its creation time (a child forked after its snapshot), since `open`
//!    already proved it a child of a live member. Then `SIGKILL` each doomed
//!    member through its pidfd — every descendant first, the root last —
//!    `SIGCONT` each member this walk stopped and did not doom (only a bare
//!    finalize with no walk, or a drop after an unwind, leaves any), and
//!    rebuild `report.killed` from the kills actually delivered.
//!
//! **It blocks.** One deadline, `budget` after the call began, bounds it:
//! no new pass starts past it, and each stop is confirmed by polling `/proc`
//! for at most `STOP_CONFIRM`, never past it. Only an empty first pass sleeps
//! (10 ms) before the next. A pass that has started runs to its end, so a
//! call takes at most `budget` plus one pass's `/proc` reads and signals,
//! plus the finalizer's. An async caller runs it under `spawn_blocking`.
//!
//! **Why this is sound.** The walk opens a parent before its children, and
//! every member is stopped as it is opened: `open` sends `SIGSTOP` and then
//! waits, for at most `STOP_CONFIRM` (200 ms, never past the walk's deadline),
//! until every thread of the process is in state `T`. A stopped member cannot
//! exit on its own and a stopped parent cannot reap, so in the ordinary case
//! every member's pid stays
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
//! ended, the log names the member, and the report says
//! `recheck_snapshot_failed`. A process whose `/proc` entry or pidfd
//! cannot be read (an errno other than "no such process") is logged with its
//! errno and makes the report say `recheck_snapshot_failed`, so a walk that may
//! have missed a descendant never reports itself complete. A pid freed by an
//! exited member and reused, mid-walk, by a genuine new descendant is offered
//! by `process_tree::walk` only once per pid, so such a process may not be
//! walked; the snapshot notices it and the report says
//! `recheck_snapshot_failed`. Whether a member is resumed is decided from
//! the stat read taken just BEFORE this walk's `SIGSTOP`: a job-control stop
//! (or `SIGCONT`) from someone else landing between that read and the
//! `SIGSTOP` is misread, so such a member may be resumed when it had been
//! stopped, or left stopped when it had just been continued. A member that
//! someone else kills or continues mid-walk is noticed — the snapshot sees
//! the exit, the finalizer re-checks that every member is still stopped —
//! and the report says `recheck_snapshot_failed`; a member continued and
//! stopped again between two looks is not noticed. If THIS process dies
//! mid-walk (a crash, a `SIGKILL`), no finalizer runs: every member it
//! stopped stays stopped until someone sends it `SIGCONT` or kills it.

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
///
/// **`None` is sound only while the caller holds the `Child` unwaited;
/// otherwise pass the spawn-time start token.** Without a token this proves
/// only "some unreaped child of this process": a pid that was reaped and then
/// reused by ANOTHER child of this process is killed with its tree. An
/// unwaited `std`/`tokio` `Child` pins its pid (nothing reaps it), which is
/// what makes `None` safe there and nowhere else. "Unwaited" means no `wait`
/// or `try_wait` has returned its exit AND none is in flight: a `tokio`
/// `Child::wait()` future polled by another task reaps as soon as the child
/// exits, so a caller with such a future outstanding must pass the token.
///
/// `budget` sets ONE deadline for the whole call: no pass starts past it,
/// and no stop confirmation waits past it. This call BLOCKS (see the module docs); an
/// async caller wraps it in `spawn_blocking`.
///
/// `Ok` with `root_exited` and `passes == 0` when the root was already a
/// zombie. `report.killed` lists the `SIGKILL`s actually delivered, every
/// descendant before the root. `report.recheck_snapshot_failed` is also set when any
/// process's `/proc` entry or pidfd could not be read during the walk, since a
/// descendant may then have been missed. Every `Err` before the walk signalled
/// nothing.
pub fn kill_tree(
    pid: u32,
    expected_token: Option<&str>,
    budget: Duration,
) -> Result<TreeKillReport, TreeKillError> {
    kill_tree_with(pid, expected_token, budget, Handles::PidFd, confirm_stopped)
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
    confirm: Confirm,
) -> Result<TreeKillReport, TreeKillError> {
    let deadline = Instant::now() + budget;
    match freeze_root(pid, expected_token, handles, deadline, confirm)? {
        Frozen::RootExited => Ok(TreeKillReport {
            root_exited: true,
            ..TreeKillReport::default()
        }),
        Frozen::Table(table) => {
            // What is left of the one deadline, so the walk's pass check and
            // the stop confirmations share it.
            let left = deadline.saturating_duration_since(Instant::now());
            let walked = walk(&table, &ROOT, pid, std::process::id(), left);
            table.finalize_walk(walked)
        }
    }
}

/// The root's index in [`LinuxFreezeTable::members`].
const ROOT: usize = 0;

/// How a stop is confirmed: [`confirm_stopped`] in production; the tests
/// inject one that reports a stop that never took effect.
type Confirm = fn(u32, Instant) -> bool;

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
    deadline: Instant,
    confirm: Confirm,
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
        return Err(refuse(if is_no_such_process(&e) {
            format!("pid {pid} was reaped while it was being checked ({e}); nothing signalled")
        } else {
            format!("pid {pid}: its liveness could not be checked ({e}); nothing signalled")
        }));
    }
    // The token first, zombie or not: a zombie keeps its start time, and a
    // zombie that is a DIFFERENT child of this process (a reaped and reused
    // pid) is "reused", never "already exited" (review round 5).
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
    if exited_pid == spid {
        return Ok(Frozen::RootExited);
    }
    if let Err(e) = root.signal(libc::SIGSTOP) {
        return Err(refuse(format!(
            "pid {pid}: SIGSTOP refused ({e}); nothing signalled"
        )));
    }
    // A process already stopped by someone else (job control) stays stopped.
    // Decided from the stat read BEFORE the SIGSTOP: a job-control stop or
    // continue landing in between is misread (module docs, known residue).
    root.resume = !matches!(st.state, b'T' | b't');
    // A root still running can fork a child the walk never sees: the report
    // must not claim a complete walk (review round 4).
    let stopped = confirm(pid, deadline);
    Ok(Frozen::Table(LinuxFreezeTable {
        members: RefCell::new(vec![root]),
        doom_order: RefCell::new(Vec::new()),
        handles,
        deadline,
        confirm,
        incomplete: Cell::new(!stopped),
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

    /// Whether this member is alive, has exited (a zombie, or reaped), or
    /// cannot be told.
    ///
    /// Through a pidfd, the pidfd is the ONLY authority (review round 1): a
    /// `/proc` stat shows a process whose main thread called `pthread_exit` as
    /// `Z` while its other threads run, and a stat read can fail for a live
    /// process. Without a pidfd, `/proc` is all there is: absent or a
    /// different start time is exited, a `Z` leader is exited only once no
    /// thread of it is left alive (a stopped thread is alive). A failed poll
    /// or read is [`Liveness::Unknown`], and each caller decides which way an
    /// unanswered question falls (review round 2).
    fn liveness(&self) -> Liveness {
        match &self.via {
            Via::Fd(fd) => match fd.has_exited() {
                Ok(true) => Liveness::Exited,
                Ok(false) => Liveness::Alive,
                Err(e) => Liveness::Unknown(e),
            },
            Via::Pid => match proc_identity::try_stat(self.pid) {
                Ok(None) => Liveness::Exited,
                Ok(Some(s)) if s.start_ticks != self.start_ticks => Liveness::Exited,
                Ok(Some(s)) if s.zombie => match any_thread_alive(self.pid) {
                    Ok(true) => Liveness::Alive,
                    Ok(false) => Liveness::Exited,
                    Err(e) => Liveness::Unknown(e),
                },
                Ok(Some(_)) => Liveness::Alive,
                Err(e) => Liveness::Unknown(e),
            },
        }
    }

    /// For KILL decisions: exited only when provably so. An UNKNOWN member is
    /// still killed (or still counted as the root), never dropped.
    fn exited(&self) -> bool {
        matches!(self.liveness(), Liveness::Exited)
    }
}

/// What [`Member::liveness`] could establish.
#[derive(Debug)]
enum Liveness {
    Alive,
    Exited,
    /// The pidfd poll or the `/proc` read failed.
    Unknown(io::Error),
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

/// Wait, at most [`STOP_CONFIRM`] and never past the walk's `deadline`,
/// until every thread of `pid` is stopped, traced-stopped, dead, or gone.
/// `false` when the wait ran out.
fn confirm_stopped(pid: u32, deadline: Instant) -> bool {
    confirm_stopped_within(pid, deadline, STOP_CONFIRM)
}

/// [`confirm_stopped`] with its bound as a parameter: tests that assert a
/// complete walk give a loaded box longer than [`STOP_CONFIRM`].
fn confirm_stopped_within(pid: u32, deadline: Instant, bound: Duration) -> bool {
    let end = (Instant::now() + bound).min(deadline);
    loop {
        if all_threads_stopped(pid) {
            return true;
        }
        if Instant::now() >= end {
            tracing::warn!(
                target: "tree_kill",
                "pid {pid}: SIGSTOP delivered but not every thread stopped within \
                 {bound:?} (or within the walk's budget); a child it forks meanwhile is \
                 found only by a later pass"
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
    /// Indices `terminate` marked, in the walk's order. The finalizer kills
    /// them descendants first and the root last, whatever that order was.
    doom_order: RefCell<Vec<usize>>,
    handles: Handles,
    /// The walk's end (`budget` after `kill_tree` began): no stop
    /// confirmation waits past it.
    deadline: Instant,
    /// How `open` confirms a stop.
    confirm: Confirm,
    /// A process's `/proc` entry or pidfd could not be read during the walk
    /// (an errno other than "no such process"), or a doomed member's SIGKILL
    /// failed: a descendant may be missing or alive. Any unreadable entry on
    /// the box sets it, so on a host whose `/proc` refuses other users' stat
    /// files (`hidepid=1`) EVERY walk reports `recheck_snapshot_failed` — the
    /// honest answer there, since this walk cannot see those processes' parents.
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
        // parent may be a stranger's child (vet D2). One whose liveness cannot
        // be read is treated the same way for LINKING (fail closed), and the
        // walk then says it may be incomplete (review round 2).
        // A pid is a dead parent only when NO member holding it is alive: a
        // member that exited and a new member that reused its pid share it,
        // and the new one's children are the tree's (review round 3).
        let mut alive: Vec<u32> = Vec::new();
        let mut exited: Vec<(u32, u64)> = Vec::new();
        for m in self.members.borrow().iter() {
            match m.liveness() {
                Liveness::Alive => alive.push(m.pid),
                Liveness::Exited => {
                    // `open` never admits an exited process and a stopped
                    // one cannot exit by itself, so this exit came from
                    // outside the walk (a third-party SIGKILL, the OOM
                    // killer): what it forked meanwhile may be missed
                    // (review round 5).
                    if !self.incomplete.replace(true) {
                        tracing::warn!(
                            target: "tree_kill",
                            "pid {}: a member exited during the walk; the walk may be \
                             incomplete",
                            m.pid
                        );
                    }
                    exited.push((m.pid, m.start_ticks));
                }
                Liveness::Unknown(e) => {
                    exited.push((m.pid, m.start_ticks));
                    self.incomplete.set(true);
                    tracing::warn!(
                        target: "tree_kill",
                        "pid {}: liveness unreadable ({e}); its children are not linked",
                        m.pid
                    );
                }
            }
        }
        let dead_parents: Vec<u32> = exited
            .iter()
            .map(|&(pid, _)| pid)
            .filter(|pid| !alive.contains(pid))
            .collect();
        let mut rows = Vec::with_capacity(pids.len());
        let mut first_unreadable: Option<(u32, io::Error)> = None;
        let mut unreadable = 0usize;
        for pid in pids {
            match proc_identity::try_stat(pid) {
                Ok(Some(st)) => {
                    if let Ok(ppid) = u32::try_from(st.ppid) {
                        if !dead_parents.contains(&ppid) {
                            self.note_reused_member_pid(pid, &st, ppid, &alive, &exited);
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
        // Already stopped by this walk and not exited (a child the walk
        // declined on creation time in an earlier pass is offered again): the
        // same member. A member with this pid that HAS exited no longer pins
        // it, so the pid may now name a new process: opened afresh below,
        // under the same parent proof as any other.
        let known = self
            .members
            .borrow()
            .iter()
            .rposition(|m| m.pid == pid && !m.exited());
        if known.is_some() {
            return known;
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
        let mut parent_in_tree = false;
        if let Ok(ppid) = u32::try_from(st.ppid) {
            for m in self.members.borrow().iter().filter(|m| m.pid == ppid) {
                match m.liveness() {
                    Liveness::Alive => parent_in_tree = true,
                    Liveness::Exited => {}
                    // Fail closed: an unanswered liveness never links a child
                    // (review round 2).
                    Liveness::Unknown(e) => {
                        self.note_unreachable(pid, "its parent's liveness is unreadable", &e)
                    }
                }
            }
        }
        if !parent_in_tree {
            return None;
        }
        let member = Member {
            pid,
            start_ticks: st.start_ticks,
            via,
            // Read before the SIGSTOP below: the job-control race in the
            // module docs' known residue.
            resume: !matches!(st.state, b'T' | b't'),
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
        if !(self.confirm)(pid, self.deadline) {
            // Still running: it can fork a child after the walk's last
            // snapshot, so the walk may be incomplete (review round 4).
            self.incomplete.set(true);
        }
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
    /// A row whose pid an EXITED member held, but which is a different process
    /// (another start time) under a live member: a genuine new descendant at
    /// a reused pid. `process_tree::walk` offers each pid once, so it may
    /// never be opened, and the report must not claim a complete walk.
    fn note_reused_member_pid(
        &self,
        pid: u32,
        st: &proc_identity::ProcStat,
        ppid: u32,
        alive: &[u32],
        exited: &[(u32, u64)],
    ) {
        let reused = !alive.contains(&pid)
            && exited
                .iter()
                .any(|&(p, start)| p == pid && start != st.start_ticks);
        if reused && alive.contains(&ppid) {
            self.incomplete.set(true);
            tracing::warn!(
                target: "tree_kill",
                "pid {pid}: an exited member's pid now names a new child of member {ppid}; \
                 it may not be walked"
            );
        }
    }

    /// A process in (or possibly in) the tree that this walk could not reach:
    /// logged with its errno, and the report will not claim a complete walk.
    fn note_unreachable(&self, pid: u32, what: &str, e: &io::Error) {
        self.incomplete.set(true);
        tracing::warn!(
            target: "tree_kill",
            "pid {pid}: {what} ({e}); it and its subtree are not walked"
        );
    }

    /// Step 4 after a WALK: doom every member `open` stopped that the walk
    /// left undoomed, then [`Self::finalize`]. Each such member was proved, at
    /// its open, a child of a live member, so it is in the tree; the walk
    /// declines one only on its creation time — typically a child forked
    /// after the walk's snapshot, which a resume would leave running while
    /// the report read complete (review round 4). The root is doomed by the
    /// walk itself, or not at all.
    fn finalize_walk(
        &self,
        walked: Result<TreeKillReport, TreeKillError>,
    ) -> Result<TreeKillReport, TreeKillError> {
        let undoomed: Vec<usize> = self
            .members
            .borrow()
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, m)| !m.doomed)
            .map(|(i, _)| i)
            .collect();
        for i in undoomed {
            self.terminate(&i);
        }
        // Every member must still be stopped: one a third party continued
        // (SIGCONT) may have forked since the walk last looked (review round
        // 5). A member that has exited reads as stopped here — it forks
        // nothing — and its exit was already flagged by the snapshot.
        for m in self.members.borrow().iter() {
            if !all_threads_stopped(m.pid) {
                self.incomplete.set(true);
                tracing::warn!(
                    target: "tree_kill",
                    "pid {}: running again before the kill (continued from outside the \
                     walk); a child it forked meanwhile may be missed",
                    m.pid
                );
            }
        }
        self.finalize(walked)
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
    /// to: every doomed descendant in the walk's order, then the root last
    /// (`walk` dooms the root in its first pass, before members later passes
    /// find, so the walk's own order would kill it first).
    fn release(&self) -> Vec<u32> {
        if self.released.replace(true) {
            return Vec::new();
        }
        let members = self.members.borrow();
        let order: Vec<usize> = {
            let doomed = self.doom_order.borrow();
            let descendants = doomed.iter().copied().filter(|&i| i != ROOT);
            descendants
                .chain(doomed.iter().copied().filter(|&i| i == ROOT))
                .collect()
        };
        let mut killed = Vec::new();
        for &i in order.iter() {
            let m = &members[i];
            match m.signal(libc::SIGKILL) {
                Ok(()) => killed.push(m.pid),
                // Reaped, or (no pidfd) the pid now names another process.
                Err(e) if is_no_such_process(&e) => {}
                Err(e) => {
                    // Not provably gone, and possibly stopped by this walk: do
                    // not leave it frozen (review round 1, finding 3), and do
                    // not report the walk complete (review round 2).
                    self.incomplete.set(true);
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

// `cfg(test)` alone: this module is already Linux-only (lib.rs), and
// scripts/check_untimed_subprocess.py recognises exactly `#[cfg(test)]`.
/// Real-process tests. Prerequisites on PATH: `sh`, `sleep` and `true`
/// (every test); util-linux `setsid` (T2); `python3` with `ctypes` and
/// `threading` (the main-thread-exit tests). A missing one fails its test
/// with a message naming it rather than skipping, so a green run has really
/// exercised every path.
#[cfg(test)]
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

        /// The fixture's first stdout line, read on a helper thread and
        /// waited for at most 5 s, so a fixture that never prints fails the
        /// test instead of hanging it. On a timeout `Drop` kills the fixture,
        /// which closes the pipe and ends the helper thread.
        fn first_line(&mut self) -> String {
            use std::io::BufRead;
            let stdout = self.child.stdout.take().expect("piped stdout");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                let read = std::io::BufReader::new(stdout)
                    .read_line(&mut line)
                    .map(|_| line);
                let _ = tx.send(read);
            });
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(line)) => line.trim().to_string(),
                Ok(Err(e)) => panic!("reading the fixture's first line failed: {e}"),
                Err(e) => panic!("no first line from the fixture within 5 s ({e})"),
            }
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
        // Patient stop confirmation: the completeness asserts below must not
        // depend on a loaded box stopping each member within STOP_CONFIRM.
        let report =
            kill_tree_with(root, None, BUDGET, Handles::PidFd, patient).expect("tree kill");
        assert!(report.killed.contains(&root), "{report:?}");
        for (pid, _) in &tree.recorded {
            assert!(report.killed.contains(pid), "{pid} not killed: {report:?}");
        }
        // Every descendant is killed before the root (review round 3).
        assert_eq!(report.killed.last(), Some(&root), "{report:?}");
        // A clean walk says it is complete (review round 4).
        assert!(!report.budget_spent, "{report:?}");
        assert!(!report.recheck_snapshot_failed, "{report:?}");
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
        let has_setsid = probe(Command::new("sh").args(["-c", "command -v setsid"]));
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

    /// T4: a child that was already reaped signals nothing. The token is
    /// taken BEFORE the reap, as a tracked caller records it at spawn: passing
    /// `None` after a reap is exactly the unsound use `kill_tree`'s docs
    /// forbid, since the pid may meanwhile be a sibling test's fixture. Each
    /// of the three refusals — gone, not our child, or reused — is correct
    /// here; anything else, `Ok` above all, fails.
    #[test]
    fn signals_nothing_for_a_reaped_child() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        let token = proc_identity::start_token(pid).expect("an unreaped child has a token");
        child.wait().unwrap();
        let err = kill_tree(pid, Some(&token), BUDGET).expect_err("a reaped child must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("no such process")
                || msg.contains("not an unreaped child")
                || msg.contains("was reused"),
            "{err}"
        );
        assert!(msg.contains("nothing signalled"), "{err}");
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
        let Frozen::Table(table) = freeze_root(
            tree.pid(),
            None,
            Handles::PidFd,
            Instant::now() + BUDGET,
            confirm_stopped,
        )
        .expect("freeze root") else {
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
        let Frozen::Table(table) = freeze_root(
            tree.pid(),
            None,
            Handles::PidFd,
            Instant::now() + BUDGET,
            confirm_stopped,
        )
        .expect("freeze root") else {
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

    /// A table holding `members` as given — the root first — signalled by pid
    /// (`Handles::PidOnly`), so a member can be made to read as exited by
    /// recording a start time it does not have.
    fn table_of(members: Vec<Member>) -> LinuxFreezeTable {
        LinuxFreezeTable {
            members: RefCell::new(members),
            doom_order: RefCell::new(Vec::new()),
            handles: Handles::PidOnly,
            deadline: Instant::now() + BUDGET,
            confirm: confirm_stopped,
            incomplete: Cell::new(false),
            released: Cell::new(false),
        }
    }

    fn member(pid: u32, start_ticks: u64) -> Member {
        Member {
            pid,
            start_ticks,
            via: Via::Pid,
            resume: false,
            doomed: false,
        }
    }

    /// Review round 3, finding 4: a pid held by an EXITED member and a LIVE
    /// one at once (reused mid-walk) is a live parent — its children are
    /// linked — and `open` reaches the live process behind a pid an exited
    /// member once held, while the snapshot says the walk may be incomplete.
    /// The "exited" members here are the live fixture's own pids recorded
    /// with a start time they do not have, which is exactly how a reused pid
    /// reads to the table.
    #[test]
    fn a_reused_member_pid_is_neither_a_dead_parent_nor_skipped() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let (root, root_start) = (tree.pid(), tree.start());
        let (child, child_start) = tree.recorded[0];

        // The root's pid: an exited holder AND a live one.
        let table = table_of(vec![member(root, root_start + 1), member(root, root_start)]);
        let rows = table.snapshot().expect("snapshot");
        assert!(
            rows.contains(&(child, root)),
            "a live member's child was dropped as a dead parent's: {rows:?}"
        );
        drop(table);

        // The child's pid: only an exited holder, under the live root.
        let table = table_of(vec![
            member(root, root_start),
            member(child, child_start + 1),
        ]);
        table.snapshot().expect("snapshot");
        assert!(
            table.incomplete.get(),
            "a new process at an exited member's pid was not reported"
        );
        let h = table
            .open(child)
            .expect("the live process at an exited member's pid is opened afresh");
        assert_eq!(table.creation_time(&h), Some(child_start));
        wait_until("open to stop the child", || state(child) == Some(b'T'));
        drop(table);
        assert_ne!(state(child), Some(b'T'), "the child was left stopped");
        assert!(!gone(child, child_start), "the child was killed");
    }

    /// Review round 3, finding 3: `walk` dooms the root in its first pass, so
    /// a member a LATER pass finds is doomed after the root. The finalizer
    /// still kills every descendant first and the root last. Driven through
    /// `terminate` in exactly that root-first order, on a real fixture.
    #[test]
    fn finalizer_kills_the_root_last_whatever_the_doom_order() {
        // The root is not stopped here (no freeze_root), so it must outlive
        // its child by itself: `wait` returns, then it execs, and only the
        // SIGKILL ends it.
        let mut tree = Tree::sh("sleep 300 & wait; exec sleep 300", false);
        tree.await_descendants(1);
        let (root, root_start) = (tree.pid(), tree.start());
        let (child, child_start) = tree.recorded[0];
        let table = table_of(vec![member(root, root_start), member(child, child_start)]);
        assert!(table.terminate(&ROOT));
        assert!(table.terminate(&1));
        let report = table
            .finalize(Ok(TreeKillReport::default()))
            .expect("finalize");
        assert_eq!(report.killed, vec![child, root], "{report:?}");
        let status = tree.wait();
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
        assert_ends(child, child_start);
    }

    /// Whether a prerequisite probe exits 0 within 5 s. Spawned and polled
    /// with `try_wait`, so a probe that hangs fails the test instead of
    /// hanging it; one still running at the bound is killed and reaped.
    fn probe(cmd: &mut Command) -> bool {
        let Ok(mut child) = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            return false;
        };
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
            }
        }
    }

    /// Stop confirmation with a 5 s bound (still capped by the walk's
    /// deadline), for tests that assert a COMPLETE walk on a loaded box.
    fn patient(pid: u32, deadline: Instant) -> bool {
        confirm_stopped_within(pid, deadline, Duration::from_secs(5))
    }

    /// A stop confirmation that never sees the stop take effect.
    fn never_stops(_pid: u32, _deadline: Instant) -> bool {
        false
    }

    /// Review round 4, finding 1a: a member — root or opened — whose stop is
    /// not confirmed makes the walk incomplete, since it may fork a child
    /// after the last snapshot. Forced by injecting a confirmation that
    /// reports failure; a real unstoppable process (uninterruptible I/O) is
    /// not something a test can produce on demand.
    #[test]
    fn an_unconfirmed_stop_makes_the_walk_incomplete() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let (child, child_start) = tree.recorded[0];
        let deadline = Instant::now() + BUDGET;

        let Frozen::Table(table) =
            freeze_root(tree.pid(), None, Handles::PidFd, deadline, never_stops)
                .expect("freeze root")
        else {
            panic!("a live root froze as exited");
        };
        assert!(
            table.incomplete.get(),
            "an unconfirmed ROOT stop was not flagged"
        );
        let report = table
            .finalize(Ok(TreeKillReport::default()))
            .expect("finalize");
        assert!(report.recheck_snapshot_failed, "{report:?}");

        let Frozen::Table(mut table) =
            freeze_root(tree.pid(), None, Handles::PidFd, deadline, confirm_stopped)
                .expect("freeze root")
        else {
            panic!("a live root froze as exited");
        };
        assert!(!table.incomplete.get(), "a confirmed root stop was flagged");
        table.confirm = never_stops;
        table.open(child).expect("a child of the frozen root opens");
        assert!(
            table.incomplete.get(),
            "an unconfirmed MEMBER stop was not flagged"
        );
        drop(table);
        assert!(!gone(child, child_start), "a bare drop killed the child");
        assert_ne!(state(child), Some(b'T'), "the child was left stopped");
    }

    /// Review round 4, finding 1b: after a walk, a member `open` stopped but
    /// the walk declined — in the real race, a child forked after the walk's
    /// snapshot, which `admits_descendant` refuses — is KILLED, not resumed.
    /// That race is not reproducible on demand (it needs a fork to land
    /// between `now()` and the snapshot read), so this drives the table as
    /// the walk would leave it: the child opened and stopped, never doomed.
    #[test]
    fn after_a_walk_a_declined_member_is_killed_not_resumed() {
        // The root outlives its child (`wait` returns, then it execs), so a
        // root that dies here was killed, not merely left childless.
        let mut tree = Tree::sh("sleep 300 & wait; exec sleep 300", false);
        tree.await_descendants(1);
        let root = tree.pid();
        let (child, child_start) = tree.recorded[0];
        let Frozen::Table(table) = freeze_root(
            root,
            None,
            Handles::PidFd,
            Instant::now() + BUDGET,
            confirm_stopped,
        )
        .expect("freeze root") else {
            panic!("a live root froze as exited");
        };
        table.open(child).expect("a child of the frozen root opens");
        let report = table
            .finalize_walk(Ok(TreeKillReport::default()))
            .expect("finalize");
        assert_eq!(report.killed, vec![child], "{report:?}");
        assert_ends(child, child_start);
        // The root was not doomed by any walk: resumed, alive.
        std::thread::sleep(Duration::from_millis(50));
        assert!(tree.try_wait().is_none(), "the undoomed root was killed");
        assert_ne!(state(root), Some(b'T'), "the root was left stopped");
    }

    /// Review round 5, finding 1: a ZOMBIE child whose start token is not the
    /// recorded one is "reused" — a different child of this process now at
    /// that pid — never "already exited", and nothing is signalled.
    #[test]
    fn a_zombie_with_the_wrong_token_is_reused_not_exited() {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        wait_until("the child to become a zombie", || {
            stat(pid).is_some_and(|s| s.zombie)
        });
        let result = kill_tree(pid, Some("linux:not-this-boot:1"), BUDGET);
        // Reap before asserting, so a red run leaves no zombie behind.
        child.wait().unwrap();
        let err = result.expect_err("a zombie with the wrong token must be refused");
        assert!(err.to_string().contains("was reused"), "{err}");
        assert!(err.to_string().contains("nothing signalled"), "{err}");
    }

    /// Review round 5, finding 2a: a member SIGKILLed by a third party
    /// mid-walk makes the next snapshot say the walk may be incomplete.
    #[test]
    fn a_member_killed_from_outside_mid_walk_is_flagged() {
        let mut tree = Tree::sh("sleep 300 & wait", false);
        tree.await_descendants(1);
        let (child, child_start) = tree.recorded[0];
        let Frozen::Table(table) = freeze_root(
            tree.pid(),
            None,
            Handles::PidFd,
            Instant::now() + BUDGET,
            confirm_stopped,
        )
        .expect("freeze root") else {
            panic!("a live root froze as exited");
        };
        let h = table.open(child).expect("a child of the frozen root opens");
        table.snapshot().expect("snapshot");
        assert!(!table.incomplete.get(), "a clean snapshot was flagged");
        // The third party: this test, through its own pidfd.
        kill_if_same(child, child_start);
        wait_until("the killed member to read as exited", || {
            table.has_exited(&h)
        });
        table.snapshot().expect("snapshot");
        assert!(table.incomplete.get(), "an outside kill was not flagged");
    }

    /// Review round 5, finding 2b: a member a third party CONTINUED mid-walk
    /// is noticed before the kill — and still killed.
    #[test]
    fn a_member_continued_from_outside_is_flagged_and_still_killed() {
        let mut tree = Tree::sh("sleep 300 & wait; exec sleep 300", false);
        tree.await_descendants(1);
        let (child, child_start) = tree.recorded[0];
        let Frozen::Table(table) = freeze_root(
            tree.pid(),
            None,
            Handles::PidFd,
            Instant::now() + BUDGET,
            confirm_stopped,
        )
        .expect("freeze root") else {
            panic!("a live root froze as exited");
        };
        table.open(child).expect("a child of the frozen root opens");
        wait_until("open to stop the member", || state(child) == Some(b'T'));
        let Opened::Fd(fd) = pidfd::open(child as i32) else {
            panic!("pidfd of the member");
        };
        fd.send_signal(libc::SIGCONT).expect("SIGCONT");
        wait_until("the member to run again", || {
            matches!(any_thread_running(child), Ok(true))
        });
        let report = table
            .finalize_walk(Ok(TreeKillReport::default()))
            .expect("finalize");
        assert!(report.recheck_snapshot_failed, "{report:?}");
        assert!(report.killed.contains(&child), "{report:?}");
        assert_ends(child, child_start);
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
        let has_ctypes = probe(Command::new("python3").args(["-c", "import ctypes, threading"]));
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
        let report =
            kill_tree_with(root, None, BUDGET, handles, confirm_stopped).expect("tree kill");
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
        let report =
            kill_tree_with(root, None, BUDGET, Handles::PidOnly, patient).expect("tree kill");
        assert!(report.killed.contains(&root), "{report:?}");
        assert!(!report.budget_spent, "{report:?}");
        assert!(!report.recheck_snapshot_failed, "{report:?}");
        for &(pid, start) in &tree.recorded {
            assert!(report.killed.contains(&pid), "{pid} not killed: {report:?}");
            assert_ends(pid, start);
        }
    }
}
