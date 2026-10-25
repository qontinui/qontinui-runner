//! Terminate a process TREE in-process — no process is created.
//!
//! Plan `2026-10-19-served-corpus-probe-budget-excludes-process-creation`,
//! Phase 3. The runner used to end a pane's tree with `taskkill /F /T /PID`,
//! which is itself a process creation: on a box where `CreateProcessW` costs
//! seconds (and Rust std serializes every `std::process` spawn behind one
//! process-wide lock), closing one terminal took 62.7 s (diagnostic
//! `probe-runner-2026-10-18-typing-lag-is-process-creation` v1). This walk does
//! the job with a Toolhelp snapshot and `TerminateProcess`, so a close costs no
//! spawn at all.
//!
//! **Reach**: the root and every descendant the snapshot links to it, through
//! parents this walk holds a handle on. Windows keeps a dead parent's pid in
//! `th32ParentProcessID`, and that pid may name a newer, unrelated process, so
//! a process joins the tree only when its creation time is no earlier than its
//! parent's and no later than the snapshot ([`admits_descendant`]), and only
//! under a parent whose handle is still held — a pid with an open handle cannot
//! be reused, so every link is about the real parent, in every pass. Every kill
//! goes through a handle taken BEFORE the identity check, so the check and the
//! kill are about one process. The calling process is never a member and is
//! refused as a root.
//!
//! **What it misses**: a descendant already orphaned by an intermediate that
//! exited before the first snapshot, and the subtree of an intermediate this
//! process cannot open. The creation-time guard can also skip a genuine
//! descendant created in the same clock tick as a reused parent pid — the error
//! is toward signalling less.
//!
//! **Budget**: checked between passes; a pass that has started runs to its end.
//!
//! Moved here from `crates/pty-holder/src/reap.rs` (whose boot sweep reaps a
//! dead holder's surviving tree with it) so the runner's pane close and the
//! holder share ONE implementation.

use std::time::{Duration, Instant};

/// Most snapshot passes one call makes. A pass catches what the previous one's
/// kills raced with (a child forked between snapshot and kill); bounded, so a
/// fork bomb cannot hold the caller forever.
pub const TREE_KILL_PASSES: usize = 10;

/// Whether a process the snapshot listed under `parent` is still that
/// parent's child: created no earlier than the parent (a dead parent's pid may
/// since name a NEWER process) and no later than the snapshot (the listed pid
/// itself may have exited and been reused after the snapshot was taken).
pub fn admits_descendant(created: u64, parent_created: u64, snapshot_at: u64) -> bool {
    created >= parent_created && created <= snapshot_at
}

/// What one tree termination did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeKillReport {
    /// Pids `TerminateProcess` succeeded on, descendants before the root.
    pub killed: Vec<u32>,
    /// The root had already exited before this call reached it; its
    /// descendants were still walked.
    pub root_exited: bool,
    /// Snapshot passes made.
    pub passes: usize,
    /// The walk ended before it saw an empty pass: the wall-clock budget ran
    /// out, or the last of [`TREE_KILL_PASSES`] passes still found members.
    pub budget_spent: bool,
    /// A re-check pass (after the first) could not read the process snapshot,
    /// so a child forked during the kills may have been missed. The Linux
    /// table also sets it when a single process's `/proc` entry or pidfd could
    /// not be read during the walk, for the same reason.
    pub recheck_snapshot_failed: bool,
}

/// Why a tree termination failed. Typed so a caller can tell "the snapshot
/// could not be read" (the descendants' fate is UNKNOWN) from the other cases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKillError {
    /// The root is the calling process, could not be opened, or its creation
    /// time is unreadable. Nothing was signalled.
    Root(String),
    /// The first process snapshot failed: only the root was signalled
    /// (`killed` says whether that landed), and its descendants were not
    /// reached.
    SnapshotFailed {
        root_pid: u32,
        detail: String,
        killed: Vec<u32>,
    },
    /// `TerminateProcess` on the root failed and it is still running.
    RootSurvived { root_pid: u32, killed: Vec<u32> },
}

impl std::fmt::Display for TreeKillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TreeKillError::Root(why) => f.write_str(why),
            TreeKillError::SnapshotFailed {
                root_pid,
                detail,
                killed,
            } => write!(
                f,
                "pid {root_pid}: process snapshot failed ({detail}); its descendants were \
                 not reached (killed: {killed:?})"
            ),
            TreeKillError::RootSurvived { root_pid, killed } => write!(
                f,
                "pid {root_pid}: TerminateProcess failed and the process is still running \
                 (descendants killed: {killed:?})"
            ),
        }
    }
}

impl std::error::Error for TreeKillError {}

/// What the walk needs from the operating system. A trait so the linking rule
/// and the pass loop are tested against a scripted process table — pid reuse
/// included — on every OS; the Windows implementation is [`Win32Table`], the
/// Linux one is `qontinui_pty_holder::tree_freeze::LinuxFreezeTable`.
pub trait ProcessTable {
    /// An owned process handle. While one is held, its pid cannot be reused.
    ///
    /// Windows gets that from the kernel: an open process handle keeps the pid
    /// reserved. Linux has no such rule — a pid is free the moment its process
    /// is reaped — so the Linux table STOPS each process as it opens it
    /// (`SIGSTOP` through a pidfd), and `terminate` only marks it, deferring
    /// every kill until the walk has returned. That keeps a member from
    /// exiting on its own and a stopped parent from reaping, but it does not
    /// reserve the pid outright: a member someone else `SIGKILL`s under a
    /// parent that ignores `SIGCHLD` is reaped by the kernel at once, and the
    /// root's waiter is the caller itself. What keeps a stranger out there is
    /// the Linux table's `open`: it reads a candidate's parent only after
    /// taking the candidate's pidfd, requires that parent to be a member whose
    /// own pidfd says it is still alive, and its snapshot drops the rows whose
    /// parent member has exited.
    type Handle;
    /// "Now", in the units creation times are compared in (FILETIME).
    fn now(&self) -> u64;
    /// `(pid, parent pid)` of every live process.
    fn snapshot(&self) -> std::io::Result<Vec<(u32, u32)>>;
    /// Open `pid` for query, terminate and wait; `None` when it cannot be.
    fn open(&self, pid: u32) -> Option<Self::Handle>;
    /// The creation time of the process behind `h`.
    fn creation_time(&self, h: &Self::Handle) -> Option<u64>;
    /// Terminate the process behind `h`; `true` when the kill was delivered.
    fn terminate(&self, h: &Self::Handle) -> bool;
    /// Whether the process behind `h` has exited.
    fn has_exited(&self, h: &Self::Handle) -> bool;
}

/// Terminate the process behind `root` (pid `root_pid`) and every descendant,
/// descendants first, in at most [`TREE_KILL_PASSES`] passes and no new pass
/// once `budget` has run out. `self_pid` is never signalled.
///
/// Every member's handle is held until the call returns, so a member killed in
/// one pass still pins its pid in the next: a process the snapshot lists under
/// that pid is the member's own child, never a stranger's that reused the pid.
/// On Linux a handle does not pin its pid by itself (see
/// [`ProcessTable::Handle`]): the table stops each member as it opens it — the
/// walk opens a parent before its children — so a member normally neither
/// exits nor reaps during the walk, and the deferred kills land after it
/// returns. A member freed by someone else mid-walk is kept from linking a
/// stranger by that table's own liveness check on the parent, not by this
/// function.
///
/// `Err` when the root is `self_pid`, when its creation time is unreadable,
/// when the FIRST snapshot fails (after signalling the root alone, so the
/// caller learns the descendants were not reached), or when the root is still
/// alive at the end. A later pass's failed snapshot only ends the re-checks and
/// is reported as [`TreeKillReport::recheck_snapshot_failed`].
pub fn walk<T: ProcessTable>(
    table: &T,
    root: &T::Handle,
    root_pid: u32,
    self_pid: u32,
    budget: Duration,
) -> Result<TreeKillReport, TreeKillError> {
    if root_pid == self_pid {
        return Err(TreeKillError::Root(format!(
            "pid {root_pid}: refusing to terminate the calling process's own tree"
        )));
    }
    let root_created = table
        .creation_time(root)
        .ok_or_else(|| TreeKillError::Root(format!("pid {root_pid}: creation time unreadable")))?;
    let end = Instant::now() + budget;
    let mut report = TreeKillReport {
        root_exited: table.has_exited(root),
        ..TreeKillReport::default()
    };
    // Every member found so far, WITH its handle: held for the whole call.
    let mut members: Vec<(u32, u64, T::Handle)> = Vec::new();
    let mut root_signalled = report.root_exited;
    for pass in 0..TREE_KILL_PASSES {
        report.passes = pass + 1;
        // Taken BEFORE the snapshot: a listed pid reused after this instant
        // names a process created after it, and is refused.
        let snapshot_at = table.now();
        let procs = match table.snapshot() {
            Ok(procs) => procs,
            Err(e) if pass == 0 => {
                if !root_signalled && table.terminate(root) {
                    report.killed.push(root_pid);
                }
                return Err(TreeKillError::SnapshotFailed {
                    root_pid,
                    detail: e.to_string(),
                    killed: report.killed,
                });
            }
            Err(_) => {
                report.recheck_snapshot_failed = true;
                break;
            }
        };
        let first_new = members.len();
        // Breadth-first over the root and every member (old and new): an old
        // member's pid is still pinned by its handle, so a child it forked
        // after the previous snapshot is linked here.
        let mut next_parent = 0;
        loop {
            let (parent, parent_created) = match next_parent {
                0 => (root_pid, root_created),
                i => match members.get(i - 1) {
                    Some(m) => (m.0, m.1),
                    None => break,
                },
            };
            next_parent += 1;
            for &(pid, ppid) in &procs {
                if ppid != parent
                    || pid == parent
                    || pid == self_pid
                    || pid == root_pid
                    || members.iter().any(|m| m.0 == pid)
                {
                    continue;
                }
                let Some(h) = table.open(pid) else {
                    continue;
                };
                match table.creation_time(&h) {
                    Some(created) if admits_descendant(created, parent_created, snapshot_at) => {
                        members.push((pid, created, h))
                    }
                    _ => {}
                }
            }
        }
        // Descendants first, deepest last-found first.
        for (pid, _, h) in members[first_new..].iter().rev() {
            if table.terminate(h) {
                report.killed.push(*pid);
            }
        }
        let found_any = members.len() > first_new;
        // The pass cap is about to end a walk that is still finding members:
        // as unfinished as a spent budget, and reported the same way.
        if found_any && pass + 1 == TREE_KILL_PASSES {
            report.budget_spent = true;
        }
        if !root_signalled {
            root_signalled = true;
            if table.terminate(root) {
                report.killed.push(root_pid);
            } else if table.has_exited(root) {
                report.root_exited = true;
            }
        }
        if !found_any && pass > 0 {
            break;
        }
        if Instant::now() >= end {
            report.budget_spent = true;
            break;
        }
        if !found_any {
            // Pass 0 found nothing: one more pass catches a child the root
            // forked between the snapshot and its own kill.
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if !report.root_exited && !report.killed.contains(&root_pid) && !table.has_exited(root) {
        return Err(TreeKillError::RootSurvived {
            root_pid,
            killed: report.killed,
        });
    }
    Ok(report)
}

#[cfg(windows)]
pub use imp::{
    creation_time, snapshot, terminate_process_tree, terminate_tree_of, OwnedProcess, Win32Table,
};

#[cfg(windows)]
mod imp {
    use super::{walk, ProcessTable, TreeKillError, TreeKillReport};
    use std::os::windows::io::{AsRawHandle, BorrowedHandle};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, FILETIME, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, TerminateProcess, WaitForSingleObject,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    /// The creation time of the process behind `handle`, as one FILETIME.
    ///
    /// # Safety
    /// `handle` must be a valid process handle with at least
    /// `PROCESS_QUERY_LIMITED_INFORMATION` access.
    pub unsafe fn creation_time(handle: HANDLE) -> Option<u64> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the caller guarantees `handle`; the out-pointers are valid
        // locals for the duration of the call.
        let ok =
            unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
        (ok != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }

    /// An owned process handle, closed on drop. While it is open the pid it
    /// names cannot be reused, so holding one IS the process's identity.
    #[derive(Debug)]
    pub struct OwnedProcess(HANDLE);

    // SAFETY: a process handle is a kernel object reference, usable from any
    // thread; it is only passed to thread-safe Win32 calls and closed once.
    unsafe impl Send for OwnedProcess {}
    unsafe impl Sync for OwnedProcess {}

    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            // SAFETY: a handle this type owns, closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    impl OwnedProcess {
        /// Open `pid` for query, terminate and wait.
        pub fn open(pid: u32) -> std::io::Result<Self> {
            // SAFETY: no pointers; a non-null result is owned by `Self`.
            let h = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if h.is_null() {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(Self(h))
            }
        }

        /// Duplicate a process handle someone else owns (a `Child`'s), with
        /// the same access, so it can outlive the original. Borrowed, so the
        /// original is known to be open for the duration of the call.
        pub fn duplicate(handle: BorrowedHandle<'_>) -> std::io::Result<Self> {
            let mut out: HANDLE = std::ptr::null_mut();
            // SAFETY: `handle` is a live process handle for the duration of
            // the call (the borrow says so); `out` is a valid local receiving
            // the new handle, owned by `Self`.
            let ok = unsafe {
                let me = GetCurrentProcess();
                DuplicateHandle(
                    me,
                    handle.as_raw_handle() as HANDLE,
                    me,
                    &mut out,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 || out.is_null() {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(Self(out))
            }
        }

        /// The process's creation time (FILETIME).
        pub fn creation_time(&self) -> Option<u64> {
            // SAFETY: opened (or duplicated) with query access.
            unsafe { creation_time(self.0) }
        }

        /// Whether the process has exited.
        pub fn has_exited(&self) -> bool {
            // SAFETY: a process handle with SYNCHRONIZE; a zero timeout.
            unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
        }

        /// `TerminateProcess(handle, 1)`; `true` when the kill was delivered.
        pub fn terminate(&self) -> bool {
            // SAFETY: a process handle opened with PROCESS_TERMINATE.
            unsafe { TerminateProcess(self.0, 1) != 0 }
        }
    }

    /// `(pid, parent pid)` of every process, from one Toolhelp snapshot.
    pub fn snapshot() -> std::io::Result<Vec<(u32, u32)>> {
        let mut out = Vec::new();
        // SAFETY: a plain snapshot call; the handle is closed below.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
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
        Ok(out)
    }

    /// The live Win32 [`ProcessTable`].
    pub struct Win32Table;

    impl ProcessTable for Win32Table {
        type Handle = OwnedProcess;
        fn now(&self) -> u64 {
            crate::holder_spawn::filetime_now()
        }
        fn snapshot(&self) -> std::io::Result<Vec<(u32, u32)>> {
            snapshot()
        }
        fn open(&self, pid: u32) -> Option<OwnedProcess> {
            OwnedProcess::open(pid).ok()
        }
        fn creation_time(&self, h: &OwnedProcess) -> Option<u64> {
            h.creation_time()
        }
        fn terminate(&self, h: &OwnedProcess) -> bool {
            h.terminate()
        }
        fn has_exited(&self, h: &OwnedProcess) -> bool {
            h.has_exited()
        }
    }

    /// Terminate `root_pid` and every descendant, opening the root by pid.
    ///
    /// `Err` names the pid when it cannot be opened (it does not exist, or is
    /// not ours to open). A caller that holds the root's handle — and so its
    /// identity — should use [`terminate_tree_of`] instead: a bare pid is
    /// only as good as the moment it was read.
    pub fn terminate_process_tree(
        root_pid: u32,
        budget: Duration,
    ) -> Result<TreeKillReport, TreeKillError> {
        if root_pid == std::process::id() {
            return Err(TreeKillError::Root(format!(
                "pid {root_pid}: refusing to terminate the calling process's own tree"
            )));
        }
        let root = OwnedProcess::open(root_pid).map_err(|e| {
            TreeKillError::Root(format!("pid {root_pid}: cannot open the process: {e}"))
        })?;
        terminate_tree_of(&root, root_pid, budget)
    }

    /// Terminate the process behind `root` (pid `root_pid`) and every
    /// descendant — see [`walk`](super::walk). A root that has already exited
    /// is not signalled; its descendants still are.
    pub fn terminate_tree_of(
        root: &OwnedProcess,
        root_pid: u32,
        budget: Duration,
    ) -> Result<TreeKillReport, TreeKillError> {
        walk(&Win32Table, root, root_pid, std::process::id(), budget)
    }
}

#[cfg(test)]
mod admission_tests {
    use super::admits_descendant;

    /// A pid the Toolhelp snapshot listed may have been reused after it, so a
    /// process created after the snapshot is never admitted — nor one older
    /// than the parent it supposedly has.
    #[test]
    fn process_tree_admission_bounds_creation_time() {
        let (parent, snapshot) = (1_000, 5_000);
        assert!(admits_descendant(1_000, parent, snapshot), "same instant");
        assert!(admits_descendant(3_000, parent, snapshot));
        assert!(
            admits_descendant(5_000, parent, snapshot),
            "at the snapshot"
        );
        assert!(
            !admits_descendant(999, parent, snapshot),
            "older than its parent"
        );
        assert!(
            !admits_descendant(5_001, parent, snapshot),
            "created after the snapshot"
        );
    }

    /// The walk is pure source with no process creation in it: no
    /// `std::process`, no `CreateProcess`, no shell. Read from the file itself
    /// (above this test module), so a regression that reintroduces a spawn —
    /// a `taskkill`, a `cmd /c` — cannot pass. A runtime census cannot prove
    /// this: Toolhelp does not list a process that has already exited.
    #[test]
    fn process_tree_termination_source_creates_no_process() {
        let src = include_str!("process_tree.rs");
        let production = &src[..src
            .find("#[cfg(test)]\nmod admission_tests")
            .expect("the test module marker")];
        for forbidden in [
            "std::process::Command",
            "Command::new",
            "CreateProcess",
            "ShellExecute",
            "WinExec",
            "taskkill",
        ] {
            let hits: Vec<(usize, &str)> = production
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim_start().starts_with("//"))
                .filter(|(_, l)| l.contains(forbidden))
                .collect();
            assert!(hits.is_empty(), "{forbidden} in process_tree.rs: {hits:?}");
        }
    }
}

/// The pass loop and the linking rule against a scripted process table, on
/// every OS.
#[cfg(test)]
mod walk_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[derive(Clone)]
    struct Proc {
        ppid: u32,
        created: u64,
        alive: bool,
    }

    /// A scripted reuse: the process that takes a dead pid, and its child.
    type Reuse = ((u32, Proc), (u32, Proc));

    /// A process table where a pid is reused only while NO handle to it is
    /// open — the Windows rule the walk relies on — and where a scripted
    /// reuse fires as soon as that becomes possible.
    #[derive(Default)]
    struct Fake {
        procs: RefCell<HashMap<u32, Proc>>,
        /// pid -> (the reusing process, a child of it), installed once the pid
        /// is dead and unpinned.
        reuse: RefCell<HashMap<u32, Reuse>>,
        clock: RefCell<u64>,
        terminated: RefCell<Vec<u32>>,
        /// Snapshot calls that succeed before every later one fails; `None`
        /// = never fails.
        snapshots_ok: Option<usize>,
        snapshots_taken: RefCell<usize>,
        /// A fork bomb: every snapshot first adds one NEW child of this pid,
        /// so every pass finds a member.
        forks_under: Option<u32>,
    }

    /// A handle that pins its pid until dropped.
    struct H {
        pid: u32,
        created: u64,
        pins: Rc<RefCell<HashMap<u32, usize>>>,
    }

    impl Drop for H {
        fn drop(&mut self) {
            if let Some(n) = self.pins.borrow_mut().get_mut(&self.pid) {
                *n -= 1;
            }
        }
    }

    struct Table {
        fake: Fake,
        pins: Rc<RefCell<HashMap<u32, usize>>>,
    }

    impl Table {
        fn new(fake: Fake) -> Self {
            Table {
                fake,
                pins: Rc::new(RefCell::new(HashMap::new())),
            }
        }
        fn add(&self, pid: u32, ppid: u32, created: u64) {
            self.fake.procs.borrow_mut().insert(
                pid,
                Proc {
                    ppid,
                    created,
                    alive: true,
                },
            );
        }
        fn fire_reuse(&self) {
            let pins = self.pins.borrow();
            let mut reuse = self.fake.reuse.borrow_mut();
            let ready: Vec<u32> = reuse
                .keys()
                .copied()
                .filter(|pid| {
                    let dead = self.fake.procs.borrow().get(pid).is_none_or(|p| !p.alive);
                    dead && pins.get(pid).copied().unwrap_or(0) == 0
                })
                .collect();
            for pid in ready {
                let ((p1, a), (p2, b)) = reuse.remove(&pid).unwrap();
                self.fake.procs.borrow_mut().insert(p1, a);
                self.fake.procs.borrow_mut().insert(p2, b);
            }
        }
    }

    impl ProcessTable for Table {
        type Handle = H;
        fn now(&self) -> u64 {
            // Later than every scripted creation time, and moving.
            *self.fake.clock.borrow_mut() += 1;
            1_000 + *self.fake.clock.borrow()
        }
        fn snapshot(&self) -> std::io::Result<Vec<(u32, u32)>> {
            let n = *self.fake.snapshots_taken.borrow();
            *self.fake.snapshots_taken.borrow_mut() += 1;
            if self.fake.snapshots_ok.is_some_and(|ok| n >= ok) {
                return Err(std::io::Error::other("scripted snapshot failure"));
            }
            self.fire_reuse();
            if let Some(ppid) = self.fake.forks_under {
                // Created after the root and before this snapshot's `now()`.
                self.add(10_000 + n as u32, ppid, 500);
            }
            Ok(self
                .fake
                .procs
                .borrow()
                .iter()
                .filter(|(_, p)| p.alive)
                .map(|(pid, p)| (*pid, p.ppid))
                .collect())
        }
        fn open(&self, pid: u32) -> Option<H> {
            let created = self.fake.procs.borrow().get(&pid)?.created;
            *self.pins.borrow_mut().entry(pid).or_default() += 1;
            Some(H {
                pid,
                created,
                pins: Rc::clone(&self.pins),
            })
        }
        fn creation_time(&self, h: &H) -> Option<u64> {
            Some(h.created)
        }
        fn terminate(&self, h: &H) -> bool {
            let mut procs = self.fake.procs.borrow_mut();
            match procs.get_mut(&h.pid) {
                Some(p) if p.alive && p.created == h.created => {
                    p.alive = false;
                    self.fake.terminated.borrow_mut().push(h.pid);
                    true
                }
                _ => false,
            }
        }
        fn has_exited(&self, h: &H) -> bool {
            self.fake
                .procs
                .borrow()
                .get(&h.pid)
                .is_none_or(|p| !p.alive || p.created != h.created)
        }
    }

    const SELF: u32 = 4;
    const BUDGET: Duration = Duration::from_secs(5);

    fn root_of(t: &Table, pid: u32) -> H {
        t.open(pid).expect("root exists")
    }

    #[test]
    fn process_tree_walk_kills_descendants_first_then_the_root() {
        let t = Table::new(Fake::default());
        t.add(100, 1, 10);
        t.add(200, 100, 20);
        t.add(300, 200, 30);
        let root = root_of(&t, 100);
        let r = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert_eq!(r.killed, vec![300, 200, 100], "{r:?}");
        assert!(!r.budget_spent, "a walk that ended on an empty pass: {r:?}");
    }

    /// A walk the pass cap ends while it is still finding members is not
    /// complete: it says so through `budget_spent`, as a spent budget does.
    #[test]
    fn process_tree_walk_flags_a_walk_the_pass_cap_ended() {
        let t = Table::new(Fake {
            forks_under: Some(100),
            ..Fake::default()
        });
        t.add(100, 1, 10);
        let root = root_of(&t, 100);
        let r = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert_eq!(r.passes, TREE_KILL_PASSES, "{r:?}");
        assert!(r.budget_spent, "{r:?}");
    }

    #[test]
    fn process_tree_walk_refuses_a_child_older_than_its_parent() {
        let t = Table::new(Fake::default());
        t.add(100, 1, 10);
        // Lists 100 as its parent, but predates it: a dead parent's pid was
        // reused by the root.
        t.add(200, 100, 5);
        let root = root_of(&t, 100);
        let r = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert_eq!(r.killed, vec![100], "{r:?}");
        assert!(!t.fake.terminated.borrow().contains(&200));
    }

    #[test]
    fn process_tree_walk_never_signals_the_calling_process() {
        let t = Table::new(Fake::default());
        t.add(100, 1, 10);
        t.add(SELF, 100, 20);
        let root = root_of(&t, 100);
        let r = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert!(!t.fake.terminated.borrow().contains(&SELF), "{r:?}");
        let me = root_of(&t, SELF);
        let err = walk(&t, &me, SELF, SELF, BUDGET).unwrap_err();
        assert!(matches!(err, TreeKillError::Root(_)), "{err}");
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    /// The pid-reuse case across passes: a member killed in pass 1 must keep
    /// its pid pinned in pass 2, or a stranger that reuses that pid — and the
    /// stranger's own child, which lists that pid as its parent and is
    /// younger than the dead member — would be linked and killed.
    #[test]
    fn process_tree_walk_never_kills_through_a_reused_member_pid() {
        let t = Table::new(Fake::default());
        t.add(100, 1, 10);
        t.add(200, 100, 20);
        // Once 200 is dead and unpinned: pid 200 is reused by an unrelated
        // process (parent 1), which has a child 900.
        t.fake.reuse.borrow_mut().insert(
            200,
            (
                (
                    200,
                    Proc {
                        ppid: 1,
                        created: 50,
                        alive: true,
                    },
                ),
                (
                    900,
                    Proc {
                        ppid: 200,
                        created: 60,
                        alive: true,
                    },
                ),
            ),
        );
        let root = root_of(&t, 100);
        let r = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert!(r.passes >= 2, "{r:?}");
        let terminated = t.fake.terminated.borrow();
        assert!(
            !terminated.contains(&900),
            "killed a stranger's child: {r:?}"
        );
        assert!(
            terminated.iter().filter(|p| **p == 200).count() <= 1,
            "killed the stranger that reused pid 200: {r:?}"
        );
    }

    #[test]
    fn process_tree_walk_reports_a_failed_snapshot_after_signalling_the_root() {
        let t = Table::new(Fake {
            snapshots_ok: Some(0),
            ..Fake::default()
        });
        t.add(100, 1, 10);
        t.add(200, 100, 20);
        let root = root_of(&t, 100);
        let err = walk(&t, &root, 100, SELF, BUDGET).unwrap_err();
        assert!(
            matches!(&err, TreeKillError::SnapshotFailed { killed, .. } if *killed == vec![100]),
            "{err}"
        );
        assert_eq!(*t.fake.terminated.borrow(), vec![100]);
    }

    /// A snapshot that fails in a RE-CHECK pass (after the first pass found
    /// and killed the tree) is not an error: the kills stand, and the report
    /// says the re-check could not run.
    #[test]
    fn process_tree_walk_flags_a_failed_recheck_snapshot() {
        let t = Table::new(Fake {
            snapshots_ok: Some(1),
            ..Fake::default()
        });
        t.add(100, 1, 10);
        t.add(200, 100, 20);
        let root = root_of(&t, 100);
        let r = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert_eq!(r.killed, vec![200, 100], "{r:?}");
        assert!(r.recheck_snapshot_failed, "{r:?}");
    }

    #[test]
    fn process_tree_walk_repeated_on_an_exited_root_is_a_no_op() {
        let t = Table::new(Fake::default());
        t.add(100, 1, 10);
        let root = root_of(&t, 100);
        walk(&t, &root, 100, SELF, BUDGET).unwrap();
        let again = walk(&t, &root, 100, SELF, BUDGET).unwrap();
        assert!(again.root_exited && again.killed.is_empty(), "{again:?}");
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// Fixture processes, each assigned to a kill-on-close job right after
    /// its spawn: dropping the fixture ends them and everything they started
    /// once assigned, so a failed assertion leaves nothing running (bar a
    /// child started in the instant before the assignment).
    struct Fixture {
        children: Vec<Child>,
        job: crate::ScopedKillOnCloseJob,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                children: Vec::new(),
                job: crate::ScopedKillOnCloseJob::create(None).expect("job"),
            }
        }

        fn spawn(&mut self, args: &[&str]) -> &mut Child {
            let child = Command::new(args[0])
                .args(&args[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn fixture");
            // SAFETY: `child` is alive and owns its handle for this call.
            unsafe { self.job.assign(child.as_raw_handle() as _) };
            self.children.push(child);
            self.children.last_mut().expect("just pushed")
        }
    }

    /// Every pid `snapshot` links below `root` (no identity check — test use).
    fn descendants(root: u32) -> Vec<u32> {
        let procs = snapshot().expect("snapshot");
        let mut tree = vec![root];
        let mut i = 0;
        while i < tree.len() {
            let parent = tree[i];
            for &(pid, ppid) in &procs {
                if ppid == parent && pid != parent && !tree.contains(&pid) {
                    tree.push(pid);
                }
            }
            i += 1;
        }
        tree.remove(0);
        tree
    }

    /// Wait until the tree under `root` has `n` descendants.
    fn wait_for_descendants(root: u32, n: usize) -> Vec<u32> {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let d = descendants(root);
            if d.len() >= n {
                return d;
            }
            assert!(Instant::now() < end, "tree never grew: {d:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Handles on `pids`, taken now, so a later liveness check cannot be
    /// fooled by a reused pid.
    fn watch(pids: &[u32]) -> Vec<OwnedProcess> {
        pids.iter()
            .filter_map(|p| OwnedProcess::open(*p).ok())
            .collect()
    }

    fn wait_gone(p: &OwnedProcess) {
        let end = Instant::now() + Duration::from_secs(5);
        while !p.has_exited() {
            assert!(Instant::now() < end, "a tree member survived");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn process_tree_a_three_level_tree_is_gone_after_one_call() {
        // `cmd /c cmd /c ping -n 60 127.0.0.1`: a three-level tree.
        let mut fx = Fixture::new();
        let pid = fx
            .spawn(&["cmd", "/c", "cmd", "/c", "ping", "-n", "60", "127.0.0.1"])
            .id();
        let below = wait_for_descendants(pid, 2);
        let watched = watch(&below);
        let root = OwnedProcess::duplicate(fx.children[0].as_handle()).unwrap();
        let report = terminate_tree_of(&root, pid, Duration::from_secs(5)).unwrap();
        assert!(report.killed.contains(&pid), "{report:?}");
        for b in &below {
            assert!(report.killed.contains(b), "{b} missing from {report:?}");
        }
        for w in &watched {
            wait_gone(w);
        }
        let _ = fx.children[0].wait();
        // Repetition: the same root again is a no-op, not an error.
        let again = terminate_tree_of(&root, pid, Duration::from_secs(5)).unwrap();
        assert!(again.root_exited, "{again:?}");
        assert!(again.killed.is_empty(), "{again:?}");
    }

    /// The root has exited on its own: its live descendants are still ended,
    /// and the report says the root was already gone.
    #[test]
    fn process_tree_an_exited_root_still_has_its_descendants_reaped() {
        // `cmd /c start /b ping ...` exits at once and leaves ping running
        // with the exited cmd as its recorded parent.
        let mut fx = Fixture::new();
        let pid = fx
            .spawn(&["cmd", "/c", "start", "/b", "ping", "-n", "60", "127.0.0.1"])
            .id();
        let root = OwnedProcess::duplicate(fx.children[0].as_handle()).unwrap();
        let below = wait_for_descendants(pid, 1);
        let watched = watch(&below);
        let _ = fx.children[0].wait();
        assert!(root.has_exited());
        let report = terminate_tree_of(&root, pid, Duration::from_secs(5)).unwrap();
        assert!(report.root_exited, "{report:?}");
        for b in &below {
            assert!(report.killed.contains(b), "{b} missing from {report:?}");
        }
        for w in &watched {
            wait_gone(w);
        }
    }

    #[test]
    fn process_tree_a_pid_that_does_not_exist_is_an_error_naming_it() {
        // Far above any pid Windows allocates (the kernel ignores a pid's low
        // two bits, so the value is not one by being odd — by being huge).
        let err = terminate_process_tree(4_294_967_293, Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("4294967293"), "{err}");
    }

    #[test]
    fn process_tree_refuses_the_calling_process() {
        let err = terminate_process_tree(std::process::id(), Duration::from_secs(1))
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing"), "{err}");
    }
}
