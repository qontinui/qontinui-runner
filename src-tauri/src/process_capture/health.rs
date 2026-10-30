//! Port-based health checking for managed processes.

use std::cell::Cell;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use qontinui_runner_win32::process_tree::{walk, ProcessTable, TreeKillError};

use super::process_table::ListenerOwner;
use super::process_tree::{PidWithSpawnedAt, ProcessSnapshot};
use crate::process_helpers::{run_probe_quiet, ProbeOutcome};

// ── What creates a process here, and the budgets for what still does ────────
//
// On Windows no helper here creates a process itself (plan
// `2026-10-28-runner-spawn-bursts-overwhelm-the-box-beyond-the-continuation-cap`).
// `pid_alive` and `is_port_in_use` never did. `port_owner_pid` reads the shared
// in-process listener table (`super::process_table::port_owner`); the one spawn
// reachable from here is that read's `Get-NetTCPConnection` fallback inside
// `process_table`, behind its circuit, taken only when the in-process table is
// unreadable. `kill_descendant_tree` and `kill_port_process` end process trees
// in-process ([`kill_tree_if_same`], [`kill_port_owner`], over #2386's Toolhelp
// walk), and the port kill reads its owner without that fallback
// (`process_table::port_listener_in_process`), as does the reconcile loop's
// per-tick owner-change probe (`listener_pid_in_process`).
//
// What still shells out is the Unix side: `port_owner_pid` (`ss`, `lsof`) and
// `kill_port_process` (`lsof`, `kill`). Those run from the per-managed-process
// health loop, i.e. repeatedly and on a timer. Before the 2026-08-30 fix they
// were bare `Command::output()` calls: an `lsof` (or, then, a `powershell`)
// that never returned parked the calling thread forever, and since the loop
// re-fires per process per tick the parked threads accumulated until tokio's
// blocking pool was exhausted and the whole runner wedged. Every external call
// here is therefore bounded, and every degrade path is the one the caller
// already had (`None` / `false` / `0`).

/// Budget for reading a connection table through an external tool (Unix:
/// `ss`, `lsof`). These are pure reads of kernel state: a healthy one is
/// milliseconds, and the pathologies (`lsof` blocking on an unresponsive mount)
/// do not resolve with more waiting.
const TABLE_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Budget for a kill (the in-process tree kill on Windows, `kill` on Unix).
/// Killing is fast when it works at all; a kill that has not returned in 10s is
/// not going to, and the caller's fallback path is better than a parked thread.
const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// Pids a port kill never signals, whatever the listener table names: 0 is the
/// System Idle Process and 4 is System (which owns HTTP.sys listeners). This
/// runner's own pid is refused beside them.
const PROTECTED_PIDS: [u32; 2] = [0, 4];

/// Cheaply check whether a PID is still alive. Used by the port_health_loop
/// to detect inner service-worker death while the outer `cmd.exe` shim is
/// still running. Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
/// + `GetExitCodeProcess`; Unix: `kill(pid, 0)`.
///
/// Returns `false` for PID 0, for permission-denied, and for any PID that
/// has exited or never existed.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code as *mut u32);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(not(windows))]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // `kill(pid, 0)` returns 0 if the process exists and we have permission
    // to signal it; -1 + ESRCH if it does not exist.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Whether the live process behind `record.pid` is still the one `record`
/// tracks, given its creation time as read now (`live_unix`, and the exact
/// FILETIME `live_ft` when the platform has one): a record with no creation
/// time cannot be judged and passes on liveness alone; an unreadable live
/// time is not proven to be the same process and fails; otherwise
/// [`is_same_process`]. Pure, so the identity rule is tested on every OS.
pub(crate) fn tracked_is_same(
    record: PidWithSpawnedAt,
    live_unix: Option<i64>,
    live_ft: Option<u64>,
) -> bool {
    if record.spawned_at_unix <= 0 && record.spawned_at_ft.unwrap_or(0) == 0 {
        return true;
    }
    match live_unix {
        Some(live_unix) => is_same_process(record, live_unix, live_ft),
        None => false,
    }
}

/// Whether a `/proc/<pid>/stat` state is a live process: a zombie (`Z`)
/// or a dead task (`X`, `x`) has exited and holds nothing but its pid.
pub(crate) fn proc_state_is_alive(state: char) -> bool {
    !matches!(state, 'Z' | 'X' | 'x')
}

/// The boot time a tick's [`is_tracked_alive`] calls share, read ONCE per
/// tick: `/proc/stat`'s `btime` where `/proc` exists, `None` otherwise (and
/// always on Windows, which does not use it).
#[cfg(windows)]
pub fn proc_boot_time() -> Option<i64> {
    None
}

/// The boot time a tick's [`is_tracked_alive`] calls share, read ONCE per
/// tick: `/proc/stat`'s `btime` where `/proc` exists, `None` otherwise.
#[cfg(not(windows))]
pub fn proc_boot_time() -> Option<i64> {
    #[cfg(unix)]
    {
        super::process_tree::read_btime()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Whether the process `record` tracks is still alive AS ITSELF: running,
/// and created when the record says ([`tracked_is_same`]). A pid that died
/// and was reused reads as not alive. Windows: one
/// `PROCESS_QUERY_LIMITED_INFORMATION` handle for `GetExitCodeProcess` and
/// `GetProcessTimes` (#2386's `creation_time`); `_btime` is unused.
#[cfg(windows)]
pub fn is_tracked_alive(record: PidWithSpawnedAt, _btime: Option<i64>) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    if record.pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess takes no pointers; a non-null handle is queried
    // (both calls need only the query access asked for) and closed once.
    let (running, created) = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, record.pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let running =
            GetExitCodeProcess(handle, &mut code as *mut u32) != 0 && code == STILL_ACTIVE;
        let created = qontinui_runner_win32::process_tree::creation_time(handle);
        CloseHandle(handle);
        (running, created.filter(|ft| *ft > 0))
    };
    running
        && tracked_is_same(
            record,
            created.map(super::process_table::filetime_to_unix_secs),
            created,
        )
}

/// Whether the process `record` tracks is still alive AS ITSELF: where
/// `/proc` exists, its `/proc/<pid>/stat` state must be live (a zombie is
/// dead: [`proc_state_is_alive`]) and its start time, against the tick's
/// `btime` ([`proc_boot_time`]), must match ([`tracked_is_same`], the 2 s
/// window: those times are whole seconds); a pid that is gone, or a tick
/// with no boot time, reads as not alive. Without `/proc` (macOS) there is
/// no stat to read, so this is liveness alone ([`pid_alive`]).
#[cfg(not(windows))]
pub fn is_tracked_alive(record: PidWithSpawnedAt, btime: Option<i64>) -> bool {
    if !std::path::Path::new("/proc/self/stat").exists() {
        return pid_alive(record.pid);
    }
    #[cfg(unix)]
    let stat = btime.and_then(|btime| super::process_tree::proc_stat(record.pid, btime));
    #[cfg(not(unix))]
    let stat: Option<(char, i64)> = None;
    match stat {
        Some((state, start)) => {
            proc_state_is_alive(state) && tracked_is_same(record, Some(start), None)
        }
        None => false,
    }
}

/// Two creation times (Unix seconds) at most this far apart name the same
/// process, for a record that carries no exact FILETIME. A second-granular
/// record — the WMI fallback's `CreationDate`, a `/proc` tick count, or one
/// persisted before `PidWithSpawnedAt::spawned_at_ft` existed — cannot be
/// compared exactly; two seconds is the window the orphan reclaim has always
/// allowed.
pub(crate) const SAME_PROCESS_SKEW_SECS: u64 = 2;

/// Whether the live process (created at `live_unix`, and at the exact FILETIME
/// `live_ft` when known) is the process `recorded` names:
///
/// - both sides carry a FILETIME: they must be EQUAL. Both are the kernel's
///   own creation stamp (`GetProcessTimes`), so a reused pid differs even
///   within the same second;
/// - otherwise: both Unix seconds known (`> 0`) and at most
///   [`SAME_PROCESS_SKEW_SECS`] apart.
///
/// Fails closed on an unknown side: a pid whose creation time cannot be read
/// cannot be told from a reuse.
pub(crate) fn is_same_process(
    recorded: PidWithSpawnedAt,
    live_unix: i64,
    live_ft: Option<u64>,
) -> bool {
    match (recorded.spawned_at_ft, live_ft) {
        (Some(rec), Some(live)) => rec > 0 && rec == live,
        _ => {
            recorded.spawned_at_unix > 0
                && live_unix > 0
                && live_unix.abs_diff(recorded.spawned_at_unix) <= SAME_PROCESS_SKEW_SECS
        }
    }
}

/// What [`kill_tree_if_same`] or [`kill_port_owner`] terminated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reaped {
    /// Pids `TerminateProcess` succeeded on, descendants before the root.
    pub(crate) killed: Vec<u32>,
    /// The root was signalled and is still running; the descendants in
    /// `killed` were still ended.
    pub(crate) root_survived: bool,
    /// Tree kill only: the process table could not be read, so the
    /// handle-proven root was signalled ALONE and its descendants were not
    /// reached (the reason). `None` when the walk read the table.
    pub(crate) descendants_unreached: Option<String>,
}

/// Why [`kill_tree_if_same`], [`kill_port_owner`] or [`kill_port_owners`]
/// signalled nothing (more).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KillRefusal {
    /// Port kill: nothing listens on the port; there is nothing to kill.
    NoListener,
    /// Port kill: the listener table could not be read (UNKNOWN, as distinct
    /// from [`Self::NoListener`]); no owner is guessed.
    ListenerUnknown(String),
    /// Port kill: the listener table names a protected pid
    /// ([`PROTECTED_PIDS`], or this runner).
    Protected(u32),
    /// Port kill: the listener row carries no socket creation time (`0`), so
    /// the process now holding the owner pid cannot be proven to be the
    /// socket's owner.
    BindTimeUnknown { pid: u32 },
    /// Port kill: the pid names a process created after the socket was, so
    /// it is not the process that created it (the owner exited and the pid
    /// was reused, before or after the read).
    CreatedAfterBind { pid: u32 },
    /// Port kill loop: an owner already killed in this call still holds the
    /// listener.
    OwnerStillListening { pid: u32 },
    /// The record names no pid, or carries no creation time: a live process
    /// under that pid cannot be told from a reuse.
    RecordUnknown,
    /// The pid cannot be opened (Windows) or is not in the table (Unix): it
    /// no longer exists, or is not ours to open.
    NotOpenable,
    /// The live process's creation time cannot be read.
    LiveCreationUnknown,
    /// The pid names a different process now.
    PidReused {
        recorded: PidWithSpawnedAt,
        live_unix: i64,
        live_ft: u64,
    },
    /// The process table could not be read, or did not list this process,
    /// and nothing was signalled: the port kill never signals on it, and the
    /// tree kill's proven root had already exited or would not terminate.
    TableUnknown(String),
    /// The walk refused the root (it is this process).
    Root(String),
}

impl std::fmt::Display for KillRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KillRefusal::NoListener => f.write_str("nothing listens on the port"),
            KillRefusal::ListenerUnknown(why) => {
                write!(
                    f,
                    "the listener table is UNKNOWN ({why}); no owner is guessed"
                )
            }
            KillRefusal::Protected(pid) => write!(
                f,
                "the listener table names pid {pid}, which a port kill never signals \
                 (System Idle, System, or this runner)"
            ),
            KillRefusal::BindTimeUnknown { pid } => write!(
                f,
                "the listener row for pid {pid} carries no socket creation time, so the \
                 owner cannot be proven"
            ),
            KillRefusal::CreatedAfterBind { pid } => write!(
                f,
                "pid {pid} names a process created after the socket was (the owner exited \
                 and the pid was reused)"
            ),
            KillRefusal::OwnerStillListening { pid } => write!(
                f,
                "pid {pid} was already killed in this call and still holds the listener"
            ),
            KillRefusal::RecordUnknown => f.write_str(
                "the record carries no pid or no creation time \
                 (fail-closed against pid reuse)",
            ),
            KillRefusal::NotOpenable => f.write_str(
                "the pid cannot be opened or is not listed (it no longer exists, or is not ours)",
            ),
            KillRefusal::LiveCreationUnknown => {
                f.write_str("the live process's creation time is unreadable")
            }
            KillRefusal::PidReused {
                recorded,
                live_unix,
                live_ft,
            } => write!(
                f,
                "the pid names a different process now (recorded creation {} / {:?}, \
                 live {live_unix} / {live_ft})",
                recorded.spawned_at_unix, recorded.spawned_at_ft
            ),
            KillRefusal::TableUnknown(why) => {
                write!(
                    f,
                    "the process table is UNKNOWN ({why}); nothing was signalled"
                )
            }
            KillRefusal::Root(why) => f.write_str(why),
        }
    }
}

/// The walk's table with two guards on top:
///
/// - a snapshot that does not list this process is a failed read (the runner
///   is always live, so such a list is not the process table; the same rule
///   `process_table::validate_walk` applies to the shared table);
/// - with `hold_until_read`, nothing is terminated until a snapshot has been
///   read. #2386's walk signals the root ALONE when its first snapshot fails.
///   The port kill holds instead: an owner's descendants are tracked nowhere
///   else, and the descendant that holds the listener would be orphaned
///   beyond any later walk rooted at the owner. The tree kill does not hold:
///   its callers pass every tracked descendant as its own root, so a
///   handle-proven root signalled alone leaves nothing tracked unreached:
///   each tracked descendant gets its own kill (#2386's behaviour, reported
///   as [`Reaped::descendants_unreached`]).
///
/// The walk reads its own table at the moment of the kill, never the shared
/// memo, so there is no `stale` table here: UNKNOWN is a read that failed.
struct GuardedTable<'a, T> {
    inner: &'a T,
    self_pid: u32,
    hold_until_read: bool,
    read: Cell<bool>,
}

impl<T: ProcessTable> ProcessTable for GuardedTable<'_, T> {
    type Handle = T::Handle;
    fn now(&self) -> u64 {
        self.inner.now()
    }
    fn snapshot(&self) -> std::io::Result<Vec<(u32, u32)>> {
        let rows = self.inner.snapshot()?;
        if !rows.iter().any(|&(pid, _)| pid == self.self_pid) {
            return Err(std::io::Error::other(format!(
                "the process snapshot did not list this process (pid {})",
                self.self_pid
            )));
        }
        self.read.set(true);
        Ok(rows)
    }
    fn open(&self, pid: u32) -> Option<T::Handle> {
        self.inner.open(pid)
    }
    fn creation_time(&self, h: &T::Handle) -> Option<u64> {
        self.inner.creation_time(h)
    }
    fn terminate(&self, h: &T::Handle) -> bool {
        (!self.hold_until_read || self.read.get()) && self.inner.terminate(h)
    }
    fn has_exited(&self, h: &T::Handle) -> bool {
        self.inner.has_exited(h)
    }
}

/// End `root` and its descendants through `table` with #2386's walk
/// ([`qontinui_runner_win32::process_tree::walk`]), but only while `root.pid`
/// is still the process recorded as `root` ([`is_same_process`]: the exact
/// FILETIME when the record carries one, the 2 s window otherwise).
///
/// The root's handle is taken BEFORE the identity check and held through the
/// walk: while it is open the pid cannot be reused, so the check and every
/// kill are about one process. A descendant joins only by #2386's
/// creation-time rule (`admits_descendant`), under a parent whose handle the
/// walk holds. When the process table cannot be read, the proven root is
/// signalled alone ([`GuardedTable`] explains why that is safe here).
///
/// Generic over [`ProcessTable`] so the refusals are tested against a
/// scripted table on every OS; production passes `Win32Table`.
pub(crate) fn kill_tree_if_same<T: ProcessTable>(
    table: &T,
    root: PidWithSpawnedAt,
    self_pid: u32,
    budget: Duration,
) -> Result<Reaped, KillRefusal> {
    if root.pid == 0 || (root.spawned_at_unix <= 0 && root.spawned_at_ft.unwrap_or(0) == 0) {
        return Err(KillRefusal::RecordUnknown);
    }
    let handle = table.open(root.pid).ok_or(KillRefusal::NotOpenable)?;
    let live_ft = table
        .creation_time(&handle)
        .filter(|ft| *ft > 0)
        .ok_or(KillRefusal::LiveCreationUnknown)?;
    let live_unix = super::process_table::filetime_to_unix_secs(live_ft);
    if !is_same_process(root, live_unix, Some(live_ft)) {
        return Err(KillRefusal::PidReused {
            recorded: root,
            live_unix,
            live_ft,
        });
    }
    kill_tree_from(table, &handle, root.pid, self_pid, budget, false)
}

/// Free `port` from ONE listener: end the process that owns its LISTENING
/// socket, and that process's tree, through `table` with #2386's walk.
/// `read_listener` is the listener-table read (production:
/// `process_table::port_listener_in_process`, in-process with no external
/// fallback), returning the owner pid and the socket's own creation time.
///
/// The owner's identity comes from its own handle, proven against the
/// socket: a process created AFTER the socket cannot be the one that created
/// it, so it is refused ([`KillRefusal::CreatedAfterBind`]) whenever the owner
/// pid was reused, before the read or after it. A row with no socket creation
/// time is refused, never guessed. A protected pid ([`PROTECTED_PIDS`], or
/// `self_pid`) is refused before anything is opened, and an unreadable
/// listener or process table is refused with nothing signalled.
///
/// The listener table keeps naming an owner that has exited while a child it
/// handed the socket to still listens (measured 2026-10-30). Whether that
/// exited owner can still be opened depends on whether something else holds
/// a handle to it: on the box measured, every exited process stayed openable
/// for over a minute (held by a process that user could not inspect, socket
/// or no socket), and then the walk's exited-root path reaches the child
/// (created after the owner, before the snapshot). Where nothing holds it,
/// the owner is [`KillRefusal::NotOpenable`] and the child is not linked by
/// parent pid alone; `ManagedProcess::stop` reaps its tracked descendants
/// for that case ([`free_port_after_stop`]).
pub(crate) fn kill_port_owner<T, R>(
    table: &T,
    port: u16,
    read_listener: R,
    self_pid: u32,
    budget: Duration,
) -> Result<Reaped, KillRefusal>
where
    T: ProcessTable,
    R: FnOnce(u16) -> Result<Option<ListenerOwner>, String>,
{
    let listener = read_listener(port)
        .map_err(KillRefusal::ListenerUnknown)?
        .ok_or(KillRefusal::NoListener)?;
    let owner = listener.pid;
    if PROTECTED_PIDS.contains(&owner) || owner == self_pid {
        return Err(KillRefusal::Protected(owner));
    }
    if listener.bind_ft == 0 {
        return Err(KillRefusal::BindTimeUnknown { pid: owner });
    }
    let handle = table.open(owner).ok_or(KillRefusal::NotOpenable)?;
    let created = table
        .creation_time(&handle)
        .filter(|ft| *ft > 0)
        .ok_or(KillRefusal::LiveCreationUnknown)?;
    if created > listener.bind_ft {
        return Err(KillRefusal::CreatedAfterBind { pid: owner });
    }
    kill_tree_from(table, &handle, owner, self_pid, budget, true)
}

/// The most listener owners one port kill ends: an IPv4 and an IPv6 listener
/// can have different owners, and a worker can hold a listener for an owner
/// that was killed first. Bounded, so a respawning listener cannot hold the
/// caller.
pub(crate) const PORT_KILL_ROUNDS: usize = 4;

/// What [`kill_port_owners`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortKill {
    /// Every pid terminated, in order, across rounds.
    pub(crate) killed: Vec<u32>,
    /// The owner each round acted on.
    pub(crate) owners: Vec<u32>,
    /// Why the loop stopped: `Some(NoListener)` once nothing listens (the port
    /// is free, or never was held); any other refusal when a round refused;
    /// `None` when [`PORT_KILL_ROUNDS`] ran out.
    pub(crate) stopped_by: Option<KillRefusal>,
    /// A round's owner survived `TerminateProcess`.
    pub(crate) owner_survived: bool,
}

/// How many times, [`SAME_OWNER_REREAD_GAP`] apart, a round re-reads the
/// listener while it still names an owner this call already killed:
/// `TerminateProcess` returns before the process has torn down, and its
/// sockets close as it does, so the first re-read can still see it.
pub(crate) const SAME_OWNER_REREADS: usize = 5;

/// The gap between those re-reads (5 x 50 ms: a quarter second at most).
pub(crate) const SAME_OWNER_REREAD_GAP: Duration = Duration::from_millis(50);

/// [`kill_port_owner`] until nothing listens on `port`, a round refuses, or
/// an owner already killed in this call is still named after
/// [`SAME_OWNER_REREADS`] re-reads, at most [`PORT_KILL_ROUNDS`] rounds. Each
/// round reads the listener afresh.
pub(crate) fn kill_port_owners<T, R>(
    table: &T,
    port: u16,
    mut read_listener: R,
    self_pid: u32,
    budget: Duration,
) -> PortKill
where
    T: ProcessTable,
    R: FnMut(u16) -> Result<Option<ListenerOwner>, String>,
{
    let mut out = PortKill {
        killed: Vec::new(),
        owners: Vec::new(),
        stopped_by: None,
        owner_survived: false,
    };
    let mut read = |port| match read_listener(port) {
        Ok(Some(listener)) => Ok(listener),
        Ok(None) => Err(KillRefusal::NoListener),
        Err(why) => Err(KillRefusal::ListenerUnknown(why)),
    };
    for _ in 0..PORT_KILL_ROUNDS {
        let mut listener = match read(port) {
            Ok(listener) => listener,
            Err(why) => {
                out.stopped_by = Some(why);
                break;
            }
        };
        let mut rereads = 0;
        while out.owners.contains(&listener.pid) && rereads < SAME_OWNER_REREADS {
            std::thread::sleep(SAME_OWNER_REREAD_GAP);
            rereads += 1;
            listener = match read(port) {
                Ok(listener) => listener,
                Err(why) => {
                    out.stopped_by = Some(why);
                    return out;
                }
            };
        }
        if out.owners.contains(&listener.pid) {
            out.stopped_by = Some(KillRefusal::OwnerStillListening { pid: listener.pid });
            break;
        }
        out.owners.push(listener.pid);
        match kill_port_owner(table, port, |_| Ok(Some(listener)), self_pid, budget) {
            Ok(reaped) => {
                out.killed.extend(reaped.killed);
                out.owner_survived |= reaped.root_survived;
            }
            Err(why) => {
                out.stopped_by = Some(why);
                break;
            }
        }
    }
    out
}

/// The walk both kills share: `root` (pid `root_pid`) is a handle the caller
/// already identified and holds; the walk runs under [`GuardedTable`], holding
/// every signal until a read when `hold_until_read`.
fn kill_tree_from<T: ProcessTable>(
    table: &T,
    root: &T::Handle,
    root_pid: u32,
    self_pid: u32,
    budget: Duration,
    hold_until_read: bool,
) -> Result<Reaped, KillRefusal> {
    let gated = GuardedTable {
        inner: table,
        self_pid,
        hold_until_read,
        read: Cell::new(false),
    };
    match walk(&gated, root, root_pid, self_pid, budget) {
        Ok(report) => Ok(Reaped {
            killed: report.killed,
            root_survived: false,
            descendants_unreached: None,
        }),
        Err(TreeKillError::RootSurvived { killed, .. }) => Ok(Reaped {
            killed,
            root_survived: true,
            descendants_unreached: None,
        }),
        Err(TreeKillError::SnapshotFailed { detail, killed, .. }) if !killed.is_empty() => {
            Ok(Reaped {
                killed,
                root_survived: false,
                descendants_unreached: Some(detail),
            })
        }
        Err(TreeKillError::SnapshotFailed { detail, .. }) => Err(KillRefusal::TableUnknown(detail)),
        Err(TreeKillError::Root(why)) => Err(KillRefusal::Root(why)),
    }
}

/// What `ManagedProcess::stop` does for its health port once the root's tree
/// kill has run: wait for the port; when it is still held, reap every
/// tracked descendant concurrently (each identity-checked by
/// `kill_descendant_tree`, on Unix as on Windows; the root's tree kill cannot
/// reach a worker whose parent already exited, and a tracked worker is
/// exactly that orphan), then kill whatever still listens
/// (`kill_port_process`), then wait again. Returns whether the port is free
/// at the end; a port still held is logged at ERROR, because the next start
/// will refuse it.
///
/// Generic over the three steps so the sequence is tested without a process.
pub(crate) async fn free_port_after_stop<W, WF, R, RF, K, KF>(
    name: &str,
    port: u16,
    tracked: &[PidWithSpawnedAt],
    mut wait_free: W,
    mut reap: R,
    mut kill_port: K,
) -> bool
where
    W: FnMut(u16) -> WF,
    WF: Future<Output = bool>,
    R: FnMut(PidWithSpawnedAt) -> RF,
    RF: Future<Output = usize>,
    K: FnMut(u16) -> KF,
    KF: Future<Output = bool>,
{
    if wait_free(port).await {
        return true;
    }
    tracing::warn!(
        "Port {port} still in use after stopping '{name}': reaping {} tracked descendants, \
         then the port's owner",
        tracked.len()
    );
    // Concurrently, as `restart_process` reaps: each is an independent,
    // identity-checked kill.
    let reaped: usize = futures::future::join_all(tracked.iter().map(|root| reap(*root)))
        .await
        .into_iter()
        .sum();
    let killed = kill_port(port).await;
    if wait_free(port).await {
        tracing::info!(
            "Port {port} freed after stopping '{name}' (tracked descendants reaped: {reaped}, \
             port kill terminated a process: {killed})"
        );
        return true;
    }
    tracing::error!(
        "Port {port} is STILL held after stopping '{name}' (tracked descendants reaped: \
         {reaped}, port kill terminated a process: {killed}); the next start will refuse \
         the port"
    );
    false
}

/// The pid owning the LISTENING socket on `port`, read in-process with no
/// external fallback; `None` when nothing listens, when the table cannot be
/// read, and always off Windows. The reconcile loop's per-tick owner-change
/// probe uses it, so it must never create a process.
pub fn listener_pid_in_process(port: u16) -> Option<u32> {
    if !cfg!(windows) {
        return None;
    }
    super::process_table::port_listener_in_process(port)
        .ok()
        .flatten()
        .map(|listener| listener.pid)
}

/// Force-kill the subtree rooted at `root`, including the root, while
/// `root.pid` is still the process recorded as `root`. Used by restart-time
/// reaping and the startup orphan reclaim, which know each tracked descendant
/// (and when it was created) directly.
///
/// In-process: no process is created ([`kill_tree_if_same`] over #2386's
/// Toolhelp walk, `qontinui_runner_win32::process_tree`). The root is refused
/// unless its creation time, read on a handle held through the kill, matches
/// the record ([`is_same_process`]: exactly when the record carries a
/// FILETIME, within 2 s otherwise); a descendant joins only by #2386's
/// creation-time rule; and when the process table cannot be read, the proven
/// root is signalled alone (logged), as #2386 does.
///
/// Returns the number of processes terminated; `0` when nothing was signalled
/// (the pid no longer exists, names a different process now, or the table is
/// UNKNOWN with the root already gone; each is logged).
#[cfg(windows)]
pub async fn kill_descendant_tree(root: PidWithSpawnedAt) -> usize {
    use qontinui_runner_win32::process_tree::Win32Table;
    let outcome = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        kill_tree_if_same(&Win32Table, root, std::process::id(), KILL_TIMEOUT)
    })
    .await;
    match outcome {
        Ok(Ok(reaped)) => {
            if reaped.root_survived {
                tracing::warn!(
                    "process_capture::health: kill_descendant_tree pid {}: the root survived \
                     TerminateProcess (descendants killed: {:?})",
                    root.pid,
                    reaped.killed
                );
            }
            if let Some(why) = &reaped.descendants_unreached {
                tracing::warn!(
                    "process_capture::health: kill_descendant_tree pid {}: the process table \
                     is UNKNOWN ({why}); the proven root was ended alone",
                    root.pid
                );
            }
            reaped.killed.len()
        }
        // The common case after `stop()` already ended the tree, and the reuse
        // guard doing its job: neither is a fault.
        Ok(Err(why @ (KillRefusal::NotOpenable | KillRefusal::PidReused { .. }))) => {
            tracing::debug!(
                "process_capture::health: kill_descendant_tree pid {}: nothing killed: {why}",
                root.pid
            );
            0
        }
        Ok(Err(why)) => {
            tracing::warn!(
                "process_capture::health: kill_descendant_tree pid {}: nothing killed: {why}",
                root.pid
            );
            0
        }
        Err(e) => {
            tracing::warn!(
                "process_capture::health: kill_descendant_tree pid {}: the tree-kill task failed: {e}",
                root.pid
            );
            0
        }
    }
}

/// The pids a Unix tree kill of `root` may signal, from one fresh read of
/// the shared process table (on Linux its `/proc` walk, whose creation times
/// are whole seconds): `root.pid` only while it is still the process
/// recorded as `root` ([`is_same_process`], the 2 s window), then every
/// descendant linked to it through parents created no later than their
/// child, deepest first and the root last. A child whose creation time is
/// unreadable (`0`) is not linked, and neither is this process. An UNKNOWN
/// table (`stale`, or empty: macOS has no `/proc`) is refused.
///
/// Unix has no handle that pins a pid, so a pid can still be reused between
/// this read and the signal; that window is the time from the read to
/// `kill(2)`, not the days a tracked record can age. Platform-independent so
/// it is tested on every OS.
pub(crate) fn unix_kill_targets(
    root: PidWithSpawnedAt,
    snapshot: &ProcessSnapshot,
    self_pid: u32,
) -> Result<Vec<u32>, KillRefusal> {
    if snapshot.is_unknown() {
        return Err(KillRefusal::TableUnknown(format!(
            "stale: {}, empty: {}",
            snapshot.stale,
            snapshot.parent_map.is_empty()
        )));
    }
    if root.pid == 0 || root.spawned_at_unix <= 0 {
        return Err(KillRefusal::RecordUnknown);
    }
    if root.pid == self_pid {
        return Err(KillRefusal::Root(format!(
            "pid {}: refusing to signal the calling process's own tree",
            root.pid
        )));
    }
    let live_unix = *snapshot
        .creation_times
        .get(&root.pid)
        .ok_or(KillRefusal::NotOpenable)?;
    if live_unix <= 0 {
        return Err(KillRefusal::LiveCreationUnknown);
    }
    if !is_same_process(root, live_unix, None) {
        return Err(KillRefusal::PidReused {
            recorded: root,
            live_unix,
            live_ft: 0,
        });
    }
    let mut order = Vec::new();
    let mut seen = std::collections::HashSet::from([root.pid]);
    let mut queue = std::collections::VecDeque::from([(root.pid, live_unix)]);
    while let Some((parent, parent_created)) = queue.pop_front() {
        for &child in snapshot.parent_map.get(&parent).into_iter().flatten() {
            if child == self_pid || !seen.insert(child) {
                continue;
            }
            // Every parent here has a known (> 0) creation time, so an
            // unreadable child (`0`) fails this too.
            let created = snapshot.creation_times.get(&child).copied().unwrap_or(0);
            if created >= parent_created {
                order.push(child);
                queue.push_back((child, created));
            }
        }
    }
    order.reverse();
    order.push(root.pid);
    Ok(order)
}

/// SIGKILL `root` and its descendants ([`unix_kill_targets`]), only while
/// `root.pid` is still the process recorded as `root`, from one fresh read of
/// the shared process table (`process_table::snapshot_fresh`: on Linux the
/// in-process `/proc` walk, no child process). An UNKNOWN table refuses, and
/// is logged: on macOS, which has no `/proc`, every call refuses.
///
/// Returns the number of `kill -9` calls made; `0` when nothing was signalled.
#[cfg(not(windows))]
pub async fn kill_descendant_tree(root: PidWithSpawnedAt) -> usize {
    let snapshot = super::process_table::snapshot_fresh().await;
    let targets = match unix_kill_targets(root, &snapshot, std::process::id()) {
        Ok(targets) => targets,
        // The common case after `stop()` already ended the tree, and the reuse
        // guard doing its job: neither is a fault.
        Err(why @ (KillRefusal::NotOpenable | KillRefusal::PidReused { .. })) => {
            tracing::debug!(
                "process_capture::health: kill_descendant_tree pid {}: nothing killed: {why}",
                root.pid
            );
            return 0;
        }
        Err(why) => {
            tracing::warn!(
                "process_capture::health: kill_descendant_tree pid {}: nothing killed: {why}",
                root.pid
            );
            return 0;
        }
    };
    for pid in &targets {
        // SAFETY: kill(2) takes no pointers; a pid that exited since the read
        // fails with ESRCH, which is the outcome wanted.
        let _ = unsafe { libc::kill(*pid as i32, libc::SIGKILL) };
    }
    targets.len()
}

/// Check if a port is currently in use (accepting connections).
pub fn is_port_in_use(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let socket = match socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    ) {
        Ok(s) => s,
        Err(_) => return false,
    };

    socket
        .connect_timeout(&addr.into(), Duration::from_millis(500))
        .is_ok()
}

/// Wait for a port to become free (not accepting connections).
/// Returns true if the port is free, false if timeout elapsed.
pub async fn wait_for_port_free(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let check_interval = Duration::from_millis(200);

    loop {
        if !is_port_in_use(port) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(check_interval).await;
    }
}

/// Wait for a port to start accepting connections.
/// Returns true if the port is up, false if timeout elapsed.
pub async fn wait_for_port_ready(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let check_interval = Duration::from_millis(500);

    loop {
        if is_port_in_use(port) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(check_interval).await;
    }
}

/// Return the PID of the process that holds a LISTENING socket on `port`, or
/// `None` if no process is listening there.
///
/// # Platform behaviour
/// - **Windows**: the shared in-process read
///   ([`super::process_tree::port_owner_pid`], i.e.
///   [`super::process_table::port_owner`]): the listener-class
///   `GetExtendedTcpTable` table, IPv4 then IPv6. No process is created; the
///   `Get-NetTCPConnection` query is that read's circuit-protected fallback
///   when the table itself cannot be read.
/// - **Unix (Linux/macOS)**: tries `ss -ltnp` first (part of `iproute2`);
///   falls back to `lsof -t -i :<port>` if `ss` is unavailable or returns
///   nothing. Both tools are invoked without a shell so they are not affected
///   by PATH quirks.
///
/// Returns `None` on any error, unknown port, or permission denial.
#[cfg(windows)]
pub fn port_owner_pid(port: u16) -> Option<u32> {
    super::process_tree::port_owner_pid(port)
}

#[cfg(not(windows))]
#[expect(
    clippy::string_slice,
    reason = "legacy str byte slice — migrate to str::get / char_indices / str_utils::truncate_str; plan 2026-09-14-runner-str-byte-slice-class-has-no-lint-gate"
)]
pub fn port_owner_pid(port: u16) -> Option<u32> {
    // Try `ss -ltnp` first. Output example:
    //   State  Recv-Q Send-Q Local Address:Port  Peer Address:Port Process
    //   LISTEN 0      128    0.0.0.0:8000        0.0.0.0:*          users:(("python",pid=4321,fd=3))
    // We look for the `pid=<n>` token.
    let mut ss_cmd = crate::process_helpers::no_window("ss");
    ss_cmd.args(["-ltnp"]);
    // `run_probe_quiet`: `ss` is simply ABSENT on macOS and on minimal Linux
    // images, and exits non-zero when it has nothing to report. Both are
    // expected here — the `lsof` fallback below is the whole reason this arm is
    // allowed to fail — and this runs on the per-process capture timer, so
    // WARNing on it is pure volume that buries the timeout WARN.
    if let ProbeOutcome::Captured(raw) = run_probe_quiet(
        ss_cmd,
        TABLE_READ_TIMEOUT,
        "process_capture::health: ss port owner",
    ) {
        let text = String::from_utf8_lossy(&raw);
        let port_suffix = format!(":{}", port);
        for line in text.lines() {
            // Match lines with our port in the local address column.
            // ss prints "0.0.0.0:<port>" or "[::]:<port>".
            let has_port = line.contains(&format!(":{} ", port))
                || line.contains(&format!(":{}\t", port))
                || line.ends_with(&port_suffix);
            if !has_port {
                continue;
            }
            // Extract pid= from the process column.
            if let Some(pid_start) = line.find("pid=") {
                let after = &line[pid_start + 4..];
                let end = after
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(after.len());
                if let Ok(pid) = after[..end].parse::<u32>() {
                    if pid > 0 {
                        return Some(pid);
                    }
                }
            }
        }
    }

    // Fallback: lsof -t -i :<port>
    // `-t` emits only PIDs, one per line.
    let mut lsof_cmd = crate::process_helpers::no_window("lsof");
    lsof_cmd.args(["-t", &format!("-i:{}", port)]);
    // `run_probe_quiet`: `lsof -t -i:<port>` exits 1 whenever NOTHING owns the
    // port — the common case for a health check — and "nobody is listening" is
    // the answer, not a fault.
    let ProbeOutcome::Captured(raw) = run_probe_quiet(
        lsof_cmd,
        TABLE_READ_TIMEOUT,
        "process_capture::health: lsof port owner",
    ) else {
        return None;
    };
    let text = String::from_utf8_lossy(&raw);
    for line in text.lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            if pid > 0 {
                return Some(pid);
            }
        }
    }
    None
}

/// Free `port` by killing the process that owns its LISTENING socket, and
/// that process's tree, until nothing listens ([`kill_port_owners`]: at most
/// [`PORT_KILL_ROUNDS`] owners, an IPv4 and an IPv6 owner included).
///
/// In-process: no process is created ([`kill_port_owner`] over #2386's
/// Toolhelp walk). The owner and the socket's own creation time come from
/// the in-process listener read alone
/// (`process_table::port_listener_in_process`). When that table is
/// unreadable, or the `QONTINUI_PROCESS_TABLE_IN_PROCESS=0` kill switch is
/// set, nothing is killed and a WARN says why: the kill path creates no
/// process, and the `Get-NetTCPConnection` / `netstat` fallbacks this used to
/// spawn are gone. An owner created after the socket (a reused pid) is
/// refused, System Idle (0), System (4) and this runner are never signalled,
/// and a descendant joins only by #2386's creation-time rule.
///
/// Returns `true` when at least one process was terminated. Callers:
/// `manager::start_process` ignores it and waits for the port;
/// `ManagedProcess::stop` reaches it through [`free_port_after_stop`], which
/// ignores it too and waits for the port afterwards.
#[cfg(windows)]
pub async fn kill_port_process(port: u16) -> bool {
    use qontinui_runner_win32::process_tree::Win32Table;
    let outcome = qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
        kill_port_owners(
            &Win32Table,
            port,
            super::process_table::port_listener_in_process,
            std::process::id(),
            KILL_TIMEOUT,
        )
    })
    .await;
    let kill = match outcome {
        Ok(kill) => kill,
        Err(e) => {
            tracing::warn!(
                "process_capture::health: kill_port_process port {port}: the tree-kill task failed: {e}"
            );
            return false;
        }
    };
    if kill.owner_survived {
        tracing::warn!(
            "process_capture::health: kill_port_process port {port}: an owner survived \
             TerminateProcess (owners {:?}, killed {:?})",
            kill.owners,
            kill.killed
        );
    }
    match &kill.stopped_by {
        Some(KillRefusal::NoListener) => tracing::debug!(
            "process_capture::health: kill_port_process port {port}: no listener left \
             (owners {:?}, killed {:?})",
            kill.owners,
            kill.killed
        ),
        Some(why) => tracing::warn!(
            "process_capture::health: kill_port_process port {port}: stopped (owners {:?}, \
             killed {:?}): {why}",
            kill.owners,
            kill.killed
        ),
        None => tracing::warn!(
            "process_capture::health: kill_port_process port {port}: still listening after \
             {PORT_KILL_ROUNDS} owners (owners {:?}, killed {:?})",
            kill.owners,
            kill.killed
        ),
    }
    !kill.killed.is_empty()
}

#[cfg(not(windows))]
pub async fn kill_port_process(port: u16) -> bool {
    let mut lsof_cmd = crate::process_helpers::no_window("lsof");
    lsof_cmd.args(["-ti", &format!(":{}", port)]);
    // `run_probe_quiet`: same as the probe above — `lsof -ti :<port>` exits 1
    // when nothing owns the port, i.e. when there is nothing to kill.
    let ProbeOutcome::Captured(raw) = run_probe_quiet(
        lsof_cmd,
        TABLE_READ_TIMEOUT,
        "process_capture::health: kill_port_process lsof",
    ) else {
        return false;
    };

    let stdout = String::from_utf8_lossy(&raw);
    for line in stdout.lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            if pid > 0 {
                let mut kill_cmd = crate::process_helpers::no_window("kill");
                kill_cmd.args(["-9", &pid.to_string()]);
                // `run_probe_quiet`: `kill -9` exits 1 for a pid that exited
                // between the `lsof` read and here — the same benign race.
                let _ = run_probe_quiet(kill_cmd, KILL_TIMEOUT, "process_capture::health: kill -9");
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod port_owner_tests {
    use super::*;

    /// `port_owner_pid` must return `None` for a port that nothing is
    /// listening on.  We pick a high-numbered port that is almost certainly
    /// free; if by chance it is bound the test passes vacuously (we get
    /// `Some(_)` instead of `None`, which is also valid behaviour).
    ///
    /// A positive "live port" test would require spawning a real server and
    /// is left to the integration test in `manager.rs` where the full
    /// reconcile loop is exercised end-to-end.
    #[test]
    fn free_port_returns_none_or_valid_pid() {
        // Port 49999 is in the ephemeral range and almost never bound.
        // We accept either None (expected) or Some(pid > 0) (port happened
        // to be in use on this machine at test time).
        let result = port_owner_pid(49999);
        if let Some(pid) = result {
            assert!(pid > 0, "pid must be positive if returned");
        }
        // Either outcome is acceptable — the function must not panic.
    }

    /// The Windows arm answers from the in-process listener table: a socket
    /// this test process listens on is owned by this process. A real socket
    /// and no child process; the `manager.rs` twin is `#[ignore]`d and accepts
    /// any pid.
    #[cfg(windows)]
    #[test]
    fn health_windows_port_owner_names_this_process_for_its_own_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a listener");
        let port = listener.local_addr().expect("listener address").port();
        assert_eq!(port_owner_pid(port), Some(std::process::id()));
    }
}

/// The Windows arms of the three helpers that used to create a process per
/// call (a `netstat` child for the port owner, a PowerShell/WMI child for the
/// tree kill, a PowerShell child with a `netstat` fallback for the port kill),
/// and the shared kill code they now run, create none — and the port kill's
/// listener read takes no external fallback. The Unix tree kill is pinned to
/// its identity-checked targets (`unix_kill_targets`) from one fresh table
/// read, since `stop()` reaches it too. Read from the files' own source,
/// as #2386's `process_tree_termination_source_creates_no_process` is: a
/// runtime census cannot prove the absence, because a child that already
/// exited is listed nowhere.
#[cfg(test)]
mod windows_arms_create_no_process {
    const HEALTH: &str = include_str!("health.rs");
    const PROCESS_TABLE: &str = include_str!("process_table.rs");

    /// The item whose signature line starts with `signature` (at column 0),
    /// from that line to its closing `}` at column 0, searched in the
    /// production half of `src` (above its first `#[cfg(test)]`); with
    /// `under`, only an item directly under that line (a platform arm's
    /// `#[cfg(...)]`). Panics when there is none, so a renamed or deleted item
    /// is red, not vacuous.
    fn item(src: &str, signature: &str, under: Option<&str>) -> String {
        let mut prev = "";
        let mut item: Vec<&str> = Vec::new();
        for line in src.lines().take_while(|l| l.trim() != "#[cfg(test)]") {
            if item.is_empty() {
                if line.starts_with(signature) && under.is_none_or(|u| prev.trim() == u) {
                    item.push(line);
                }
                prev = line;
                continue;
            }
            item.push(line);
            if line == "}" {
                return item.join("\n");
            }
        }
        panic!("no complete `{signature}` item (under {under:?}) in the production half");
    }

    #[test]
    fn health_windows_port_owner_and_tree_kills_create_no_process() {
        const WIN: Option<&str> = Some("#[cfg(windows)]");
        const UNIX: Option<&str> = Some("#[cfg(not(windows))]");
        let items: [(&str, &str, Option<&str>, &[&str]); 8] = [
            (
                HEALTH,
                "pub fn port_owner_pid(",
                WIN,
                &["super::process_tree::port_owner_pid(port)"],
            ),
            (
                HEALTH,
                "pub async fn kill_descendant_tree(",
                WIN,
                &["kill_tree_if_same(", "Win32Table"],
            ),
            (
                HEALTH,
                "pub async fn kill_descendant_tree(",
                UNIX,
                // The identity-checked targets, from one fresh table read.
                &["unix_kill_targets(", "process_table::snapshot_fresh()"],
            ),
            (
                HEALTH,
                "pub(crate) fn unix_kill_targets(",
                None,
                &["is_same_process(", "is_unknown()"],
            ),
            (
                HEALTH,
                "pub async fn kill_port_process(",
                WIN,
                // The owner read with NO external fallback: the kill path
                // creates no process.
                &[
                    "kill_port_owners(",
                    "Win32Table",
                    "process_table::port_listener_in_process",
                ],
            ),
            (
                HEALTH,
                "pub(crate) fn kill_port_owner<",
                None,
                &["kill_tree_from(", "bind_ft"],
            ),
            (HEALTH, "fn kill_tree_from<", None, &["walk(&gated"]),
            (
                PROCESS_TABLE,
                "pub(crate) fn port_listener_in_process(",
                None,
                &["platform::listening_port_listener("],
            ),
        ];
        for (src, signature, under, needed) in items {
            let item = item(src, signature, under);
            // The positive half first: an absence alone would pass on an item
            // that does nothing at all.
            for needed in needed {
                assert!(
                    item.contains(needed),
                    "`{signature}` no longer goes through `{needed}`:\n{item}"
                );
            }
            let code = item
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n")
                .to_ascii_lowercase();
            for forbidden in [
                "command",
                "no_window",
                "run_probe",
                "powershell",
                "pwsh",
                "netstat",
                "get-ciminstance",
                "get-nettcpconnection",
                "stop-process",
                "taskkill",
                "fallback",
            ] {
                assert!(
                    !code.contains(forbidden),
                    "`{forbidden}` in `{signature}`: it creates a process again:\n{item}"
                );
            }
        }
    }
}

/// The kills against a scripted table, on every OS: the root's identity,
/// the table-readability guard, the descendant rule, the port owner's proof
/// against the socket, the owner loop, and `stop()`'s port sequence — with no
/// real process.
#[cfg(test)]
mod kill_tree_tests {
    use super::*;
    use crate::process_capture::process_table::unix_secs_to_filetime;
    use std::cell::RefCell;

    /// This process, as the scripted table lists it.
    const SELF: u32 = 1;
    /// The Unix second the recorded root was created in.
    const T: i64 = 1_715_911_000;
    const BUDGET: Duration = Duration::from_secs(5);
    const PORT: u16 = 41_234;

    fn ft(unix: i64) -> u64 {
        unix_secs_to_filetime(u64::try_from(unix).expect("a positive Unix second"))
    }

    /// The root (10), its child (11) and grandchild (12), and a stranger (13)
    /// that lists the root's pid as its parent but is older than the root: its
    /// real parent died and the pid was reused by the root.
    fn tree() -> Vec<(u32, u32, u64)> {
        vec![
            (10, 5, ft(T)),
            (11, 10, ft(T + 1)),
            (12, 11, ft(T + 2)),
            (13, 10, ft(T - 100)),
        ]
    }

    /// A second-granular record (no FILETIME) of pid 10.
    fn record(spawned_at_unix: i64) -> PidWithSpawnedAt {
        PidWithSpawnedAt {
            pid: 10,
            spawned_at_unix,
            spawned_at_ft: None,
        }
    }

    /// The listener on `PORT`, owned by `pid`, its socket created 46 ms after
    /// the root (the gap measured between a process and its listener).
    fn listener(pid: u32) -> ListenerOwner {
        ListenerOwner {
            pid,
            bind_ft: ft(T) + 460_516,
        }
    }

    /// How the scripted table answers a snapshot.
    #[derive(Clone, Copy)]
    enum Snap {
        Readable,
        Fails,
        OmitsSelf,
    }

    /// Rows are `(pid, parent pid, creation FILETIME)`; a terminated process
    /// leaves the table. A handle is `(pid, creation)`: the identity an open
    /// handle pins. An `unkillable` pid refuses `terminate`; an `exited` pid
    /// is a zombie — openable, already exited, and (like Toolhelp) not listed.
    struct Scripted {
        rows: RefCell<Vec<(u32, u32, u64)>>,
        snap: Snap,
        unkillable: Vec<u32>,
        exited: Vec<u32>,
        terminated: RefCell<Vec<u32>>,
    }

    impl Scripted {
        fn new(snap: Snap, rows: &[(u32, u32, u64)]) -> Self {
            let mut all = vec![(SELF, 0, ft(T - 10_000))];
            all.extend_from_slice(rows);
            Scripted {
                rows: RefCell::new(all),
                snap,
                unkillable: Vec::new(),
                exited: Vec::new(),
                terminated: RefCell::default(),
            }
        }

        fn terminated(&self) -> Vec<u32> {
            self.terminated.borrow().clone()
        }

        fn listed(&self, pid: u32) -> bool {
            self.rows.borrow().iter().any(|r| r.0 == pid)
        }
    }

    impl ProcessTable for Scripted {
        type Handle = (u32, u64);
        fn now(&self) -> u64 {
            ft(T + 3_600)
        }
        fn snapshot(&self) -> std::io::Result<Vec<(u32, u32)>> {
            let rows = self.rows.borrow();
            let live = rows.iter().filter(|r| !self.exited.contains(&r.0));
            match self.snap {
                Snap::Fails => Err(std::io::Error::other(
                    "scripted: the snapshot is unreadable",
                )),
                Snap::Readable => Ok(live.map(|r| (r.0, r.1)).collect()),
                Snap::OmitsSelf => Ok(live.filter(|r| r.0 != SELF).map(|r| (r.0, r.1)).collect()),
            }
        }
        fn open(&self, pid: u32) -> Option<(u32, u64)> {
            self.rows
                .borrow()
                .iter()
                .find(|r| r.0 == pid)
                .map(|r| (r.0, r.2))
        }
        fn creation_time(&self, h: &(u32, u64)) -> Option<u64> {
            Some(h.1)
        }
        fn terminate(&self, h: &(u32, u64)) -> bool {
            if self.unkillable.contains(&h.0) || self.exited.contains(&h.0) {
                return false;
            }
            let mut rows = self.rows.borrow_mut();
            let before = rows.len();
            rows.retain(|r| (r.0, r.2) != *h);
            let ended = rows.len() < before;
            if ended {
                self.terminated.borrow_mut().push(h.0);
            }
            ended
        }
        fn has_exited(&self, h: &(u32, u64)) -> bool {
            self.exited.contains(&h.0) || !self.rows.borrow().iter().any(|r| (r.0, r.2) == *h)
        }
    }

    // ── kill_tree_if_same ───────────────────────────────────────────────────

    #[test]
    fn health_tree_kill_ends_the_recorded_tree_descendants_first_and_spares_a_stranger() {
        let table = Scripted::new(Snap::Readable, &tree());
        let reaped = kill_tree_if_same(&table, record(T), SELF, BUDGET)
            .expect("the recorded process, from a readable table");
        assert_eq!(
            reaped,
            Reaped {
                killed: vec![12, 11, 10],
                root_survived: false,
                descendants_unreached: None,
            }
        );
        assert_eq!(table.terminated(), vec![12, 11, 10]);
        assert!(
            table.listed(13),
            "a process older than the root is not its child"
        );
    }

    #[test]
    fn health_tree_kill_compares_an_exact_filetime_exactly() {
        // The same Unix second, one 100 ns tick apart: the 2 s window would
        // pass it, the FILETIME comparison does not.
        let exact = |created: u64| PidWithSpawnedAt {
            pid: 10,
            spawned_at_unix: T,
            spawned_at_ft: Some(created),
        };
        let table = Scripted::new(Snap::Readable, &tree());
        assert!(matches!(
            kill_tree_if_same(&table, exact(ft(T) + 1), SELF, BUDGET),
            Err(KillRefusal::PidReused { .. })
        ));
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
        let reaped =
            kill_tree_if_same(&table, exact(ft(T)), SELF, BUDGET).expect("the exact creation time");
        assert_eq!(reaped.killed, vec![12, 11, 10]);
    }

    #[test]
    fn health_tree_kill_refuses_a_pid_that_names_a_different_process_now() {
        let table = Scripted::new(Snap::Readable, &tree());
        assert!(matches!(
            kill_tree_if_same(&table, record(T - 3), SELF, BUDGET),
            Err(KillRefusal::PidReused { live_unix: T, .. })
        ));
        assert_eq!(
            kill_tree_if_same(&table, record(0), SELF, BUDGET),
            Err(KillRefusal::RecordUnknown)
        );
        let no_pid = PidWithSpawnedAt {
            pid: 0,
            spawned_at_unix: T,
            spawned_at_ft: Some(ft(T)),
        };
        assert_eq!(
            kill_tree_if_same(&table, no_pid, SELF, BUDGET),
            Err(KillRefusal::RecordUnknown),
            "a record naming pid 0 is unknown, whatever its creation time"
        );
        assert!(
            table.terminated().is_empty(),
            "nothing is signalled for a pid that is not the recorded process: {:?}",
            table.terminated()
        );
    }

    #[test]
    fn health_tree_kill_signals_only_the_proven_root_when_the_table_is_unreadable() {
        for (snap, why) in [
            (Snap::Fails, "unreadable"),
            (Snap::OmitsSelf, "did not list this process"),
        ] {
            let table = Scripted::new(snap, &tree());
            let reaped = kill_tree_if_same(&table, record(T), SELF, BUDGET)
                .expect("the proven root is still ended");
            assert_eq!(reaped.killed, vec![10], "{why}");
            assert!(
                reaped
                    .descendants_unreached
                    .as_deref()
                    .is_some_and(|d| d.contains(why)),
                "{reaped:?}"
            );
            assert!(
                table.listed(11) && table.listed(12),
                "{why}: descendants are not reached"
            );
        }
    }

    #[test]
    fn health_tree_kill_reports_a_root_that_survives() {
        let mut table = Scripted::new(Snap::Readable, &tree());
        table.unkillable.push(10);
        assert_eq!(
            kill_tree_if_same(&table, record(T), SELF, BUDGET),
            Ok(Reaped {
                killed: vec![12, 11],
                root_survived: true,
                descendants_unreached: None,
            })
        );
    }

    #[test]
    fn health_tree_kill_refuses_an_unreadable_live_creation_time() {
        let table = Scripted::new(Snap::Readable, &[(10, 5, 0), (11, 10, ft(T + 1))]);
        assert_eq!(
            kill_tree_if_same(&table, record(T), SELF, BUDGET),
            Err(KillRefusal::LiveCreationUnknown)
        );
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
    }

    #[test]
    fn health_tree_kill_signals_nothing_for_an_exited_pid_or_this_process() {
        let table = Scripted::new(Snap::Readable, &tree());
        let gone = PidWithSpawnedAt {
            pid: 99,
            spawned_at_unix: T,
            spawned_at_ft: None,
        };
        assert_eq!(
            kill_tree_if_same(&table, gone, SELF, BUDGET),
            Err(KillRefusal::NotOpenable)
        );
        let me = PidWithSpawnedAt {
            pid: SELF,
            spawned_at_unix: T - 10_000,
            spawned_at_ft: None,
        };
        assert!(
            matches!(
                kill_tree_if_same(&table, me, SELF, BUDGET),
                Err(KillRefusal::Root(_))
            ),
            "this process is never a root"
        );
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
    }

    /// W2: a tracked descendant is alive only AS ITSELF. A reused pid (a
    /// different creation time) reads as dead; a record with no creation
    /// time passes on liveness; an unreadable live time is not proof.
    #[test]
    fn health_tracked_identity_reads_a_reused_pid_as_dead() {
        let exact = PidWithSpawnedAt {
            pid: 12,
            spawned_at_unix: T,
            spawned_at_ft: Some(ft(T) + 3),
        };
        assert!(tracked_is_same(exact, Some(T), Some(ft(T) + 3)));
        assert!(
            !tracked_is_same(exact, Some(T), Some(ft(T) + 4)),
            "W1 died and W2 got its pid in the same second"
        );
        assert!(tracked_is_same(record(T), Some(T + 2), None));
        assert!(
            !tracked_is_same(record(T), Some(T + 30), None),
            "a reused pid"
        );
        assert!(
            !tracked_is_same(record(T), None, None),
            "an unreadable live time"
        );
        assert!(
            tracked_is_same(record(0), None, None),
            "no recorded creation time: liveness alone decides"
        );
    }

    /// A zombie (`Z`) or a dead task (`X`) reads as dead even though its pid
    /// is still listed; running and sleeping states read as alive.
    #[test]
    fn health_a_zombie_reads_as_dead() {
        for state in ['Z', 'X', 'x'] {
            assert!(!proc_state_is_alive(state), "{state}");
        }
        for state in ['R', 'S', 'D', 'T', 'I'] {
            assert!(proc_state_is_alive(state), "{state}");
        }
    }

    #[test]
    fn health_same_process_is_exact_on_filetimes_and_within_two_seconds_otherwise() {
        let secs = |unix| PidWithSpawnedAt {
            pid: 10,
            spawned_at_unix: unix,
            spawned_at_ft: None,
        };
        // Second-granular arm.
        assert!(is_same_process(secs(T), T, None));
        assert!(is_same_process(secs(T), T + 2, Some(ft(T + 2))));
        assert!(is_same_process(secs(T), T - 2, None));
        assert!(!is_same_process(secs(T), T + 3, None));
        assert!(!is_same_process(secs(T), T - 3, None));
        assert!(!is_same_process(secs(0), 0, None), "both unknown");
        assert!(!is_same_process(secs(0), T, None), "recorded unknown");
        assert!(!is_same_process(secs(T), 0, None), "live unknown");
        // Exact arm: both sides carry a FILETIME.
        let exact = PidWithSpawnedAt {
            pid: 10,
            spawned_at_unix: T,
            spawned_at_ft: Some(ft(T) + 7),
        };
        assert!(is_same_process(exact, T, Some(ft(T) + 7)));
        assert!(
            !is_same_process(exact, T, Some(ft(T) + 8)),
            "one tick apart in the same second is a different process"
        );
        assert!(
            is_same_process(exact, T, None),
            "no live FILETIME: the 2 s window decides"
        );
    }

    // ── unix_kill_targets (the Unix tree kill) ──────────────────────────────

    /// A readable Unix table: rows are `(pid, parent pid, creation Unix
    /// seconds)`, `0` = unreadable.
    fn unix_table(rows: &[(u32, u32, i64)]) -> ProcessSnapshot {
        let mut snap = ProcessSnapshot {
            stale: false,
            ..ProcessSnapshot::default()
        };
        for &(pid, ppid, created) in rows {
            snap.parent_map.entry(ppid).or_default().push(pid);
            snap.creation_times.insert(pid, created);
        }
        snap
    }

    /// Root 10 with child 11 and grandchild 12; this process listed under 11;
    /// a stranger (13) older than the root under the root's pid; a child (14)
    /// whose creation time is unreadable.
    fn unix_tree() -> ProcessSnapshot {
        unix_table(&[
            (SELF, 11, T + 5),
            (10, 5, T),
            (11, 10, T + 1),
            (12, 11, T + 2),
            (13, 10, T - 100),
            (14, 10, 0),
        ])
    }

    #[test]
    fn health_unix_tree_kill_targets_the_recorded_tree_only() {
        assert_eq!(
            unix_kill_targets(record(T), &unix_tree(), SELF),
            Ok(vec![12, 11, 10]),
            "descendants deepest first, the root last; not this process, not a \
             stranger older than its parent, not an unreadable child"
        );
        assert_eq!(
            unix_kill_targets(record(T + 2), &unix_tree(), SELF),
            Ok(vec![12, 11, 10]),
            "a second-granular record within 2 s is the same process"
        );
    }

    #[test]
    fn health_unix_tree_kill_refuses_a_reused_pid_and_anything_unknown() {
        let tree = unix_tree();
        assert!(matches!(
            unix_kill_targets(record(T - 3), &tree, SELF),
            Err(KillRefusal::PidReused { live_unix: T, .. })
        ));
        assert_eq!(
            unix_kill_targets(record(0), &tree, SELF),
            Err(KillRefusal::RecordUnknown)
        );
        let no_pid = PidWithSpawnedAt {
            pid: 0,
            spawned_at_unix: T,
            spawned_at_ft: None,
        };
        assert_eq!(
            unix_kill_targets(no_pid, &tree, SELF),
            Err(KillRefusal::RecordUnknown)
        );
        let gone = PidWithSpawnedAt {
            pid: 99,
            spawned_at_unix: T,
            spawned_at_ft: None,
        };
        assert_eq!(
            unix_kill_targets(gone, &tree, SELF),
            Err(KillRefusal::NotOpenable)
        );
        let unreadable = unix_table(&[(SELF, 0, T - 10_000), (10, 5, 0)]);
        assert_eq!(
            unix_kill_targets(record(T), &unreadable, SELF),
            Err(KillRefusal::LiveCreationUnknown)
        );
        let me = PidWithSpawnedAt {
            pid: SELF,
            spawned_at_unix: T + 5,
            spawned_at_ft: None,
        };
        assert!(matches!(
            unix_kill_targets(me, &tree, SELF),
            Err(KillRefusal::Root(_))
        ));
        // A stale table, and the empty one a host without `/proc` (macOS)
        // produces, are UNKNOWN: nothing is a target.
        let stale = ProcessSnapshot {
            stale: true,
            ..unix_tree()
        };
        assert!(matches!(
            unix_kill_targets(record(T), &stale, SELF),
            Err(KillRefusal::TableUnknown(_))
        ));
        let no_proc = ProcessSnapshot {
            stale: false,
            ..ProcessSnapshot::default()
        };
        assert!(matches!(
            unix_kill_targets(record(T), &no_proc, SELF),
            Err(KillRefusal::TableUnknown(_))
        ));
    }

    // ── kill_port_owner ─────────────────────────────────────────────────────

    #[test]
    fn health_port_kill_ends_the_owner_tree_and_spares_a_stranger() {
        let table = Scripted::new(Snap::Readable, &tree());
        let reaped = kill_port_owner(&table, PORT, |_| Ok(Some(listener(10))), SELF, BUDGET)
            .expect("a live owner created before its socket, from readable tables");
        assert_eq!(
            reaped,
            Reaped {
                killed: vec![12, 11, 10],
                root_survived: false,
                descendants_unreached: None,
            }
        );
        assert!(
            table.listed(13),
            "a process older than the owner is not its child"
        );
    }

    #[test]
    fn health_port_kill_reaches_the_child_of_an_exited_but_openable_owner() {
        // Measured 2026-10-30: the listener table keeps naming the exited
        // owner, and the exited owner stayed openable. The walk's exited-root
        // path then ends the child that holds the socket.
        let mut table = Scripted::new(Snap::Readable, &[(10, 5, ft(T)), (11, 10, ft(T + 1))]);
        table.exited.push(10);
        let reaped = kill_port_owner(&table, PORT, |_| Ok(Some(listener(10))), SELF, BUDGET)
            .expect("an exited owner that can still be opened");
        assert_eq!(reaped.killed, vec![11]);
        assert!(!reaped.root_survived);
    }

    #[test]
    fn health_port_kill_refuses_this_process_and_system_pids() {
        // System (4) is listed and openable here, so only the refusal keeps it.
        let mut rows = tree();
        rows.push((4, 0, ft(T - 20_000)));
        for pid in [SELF, 0, 4] {
            let table = Scripted::new(Snap::Readable, &rows);
            assert_eq!(
                kill_port_owner(&table, PORT, move |_| Ok(Some(listener(pid))), SELF, BUDGET),
                Err(KillRefusal::Protected(pid)),
                "pid {pid}"
            );
            assert!(
                table.terminated().is_empty(),
                "pid {pid}: {:?}",
                table.terminated()
            );
        }
    }

    #[test]
    fn health_port_kill_refuses_an_unreadable_read() {
        // The listener table cannot be read: no owner is guessed.
        let table = Scripted::new(Snap::Readable, &tree());
        let unreadable = || "scripted: the listener table is unreadable".to_string();
        assert_eq!(
            kill_port_owner(&table, PORT, |_| Err(unreadable()), SELF, BUDGET),
            Err(KillRefusal::ListenerUnknown(unreadable()))
        );
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
        // The owner was read, but the process table cannot be: unlike the tree
        // kill, not even the owner is signalled.
        for snap in [Snap::Fails, Snap::OmitsSelf] {
            let table = Scripted::new(snap, &tree());
            assert!(matches!(
                kill_port_owner(&table, PORT, |_| Ok(Some(listener(10))), SELF, BUDGET),
                Err(KillRefusal::TableUnknown(_))
            ));
            assert!(table.terminated().is_empty(), "{:?}", table.terminated());
        }
    }

    #[test]
    fn health_port_kill_proves_the_owner_against_the_socket_creation_time() {
        // Pid 20 was created AFTER the socket: the owner that created it has
        // exited and the pid is a stranger's, whenever it was reused.
        let mut rows = tree();
        rows.push((20, 5, ft(T + 7_200)));
        let table = Scripted::new(Snap::Readable, &rows);
        assert_eq!(
            kill_port_owner(&table, PORT, |_| Ok(Some(listener(20))), SELF, BUDGET),
            Err(KillRefusal::CreatedAfterBind { pid: 20 })
        );
        // A row with no socket creation time proves nothing.
        let unbound = ListenerOwner {
            pid: 10,
            bind_ft: 0,
        };
        assert_eq!(
            kill_port_owner(&table, PORT, |_| Ok(Some(unbound)), SELF, BUDGET),
            Err(KillRefusal::BindTimeUnknown { pid: 10 })
        );
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
        // An owner created in the same tick as its socket is its owner.
        let same_tick = ListenerOwner {
            pid: 10,
            bind_ft: ft(T),
        };
        assert!(kill_port_owner(&table, PORT, |_| Ok(Some(same_tick)), SELF, BUDGET).is_ok());
    }

    #[test]
    fn health_port_kill_refuses_an_unreadable_owner_creation_time() {
        let table = Scripted::new(Snap::Readable, &[(10, 5, 0), (11, 10, ft(T + 1))]);
        assert_eq!(
            kill_port_owner(&table, PORT, |_| Ok(Some(listener(10))), SELF, BUDGET),
            Err(KillRefusal::LiveCreationUnknown)
        );
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
    }

    #[test]
    fn health_port_kill_signals_nothing_without_a_listener_or_for_an_unopenable_owner() {
        let table = Scripted::new(Snap::Readable, &tree());
        assert_eq!(
            kill_port_owner(&table, PORT, |_| Ok(None), SELF, BUDGET),
            Err(KillRefusal::NoListener)
        );
        assert_eq!(
            kill_port_owner(&table, PORT, |_| Ok(Some(listener(99))), SELF, BUDGET),
            Err(KillRefusal::NotOpenable)
        );
        assert!(table.terminated().is_empty(), "{:?}", table.terminated());
    }

    // ── kill_port_owners ────────────────────────────────────────────────────

    /// A listener read that answers from a script, one answer per call.
    fn reads(
        script: Vec<Result<Option<ListenerOwner>, String>>,
    ) -> impl FnMut(u16) -> Result<Option<ListenerOwner>, String> {
        let mut script = script.into_iter();
        move |_| script.next().unwrap_or(Ok(None))
    }

    #[test]
    fn health_port_kill_loop_ends_two_owners_then_stops_when_nothing_listens() {
        let mut rows = tree();
        rows.push((30, 5, ft(T)));
        let table = Scripted::new(Snap::Readable, &rows);
        let kill = kill_port_owners(
            &table,
            PORT,
            reads(vec![
                Ok(Some(listener(10))),
                Ok(Some(listener(30))),
                Ok(None),
            ]),
            SELF,
            BUDGET,
        );
        assert_eq!(
            kill,
            PortKill {
                killed: vec![12, 11, 10, 30],
                owners: vec![10, 30],
                stopped_by: Some(KillRefusal::NoListener),
                owner_survived: false,
            }
        );
    }

    #[test]
    fn health_port_kill_loop_stops_on_the_same_owner_a_refusal_or_the_round_cap() {
        // The same owner named again after its kill.
        let table = Scripted::new(Snap::Readable, &tree());
        let kill = kill_port_owners(&table, PORT, |_| Ok(Some(listener(10))), SELF, BUDGET);
        assert_eq!(kill.killed, vec![12, 11, 10]);
        assert_eq!(
            kill.stopped_by,
            Some(KillRefusal::OwnerStillListening { pid: 10 })
        );
        // A refusal (here: the listener read fails) ends it with nothing killed.
        let table = Scripted::new(Snap::Readable, &tree());
        let kill = kill_port_owners(
            &table,
            PORT,
            |_| Err("scripted: unreadable".to_string()),
            SELF,
            BUDGET,
        );
        assert!(kill.killed.is_empty());
        assert!(matches!(
            kill.stopped_by,
            Some(KillRefusal::ListenerUnknown(_))
        ));
        // A new owner every round: at most PORT_KILL_ROUNDS of them.
        let rows: Vec<(u32, u32, u64)> = (40..46).map(|pid| (pid, 5, ft(T))).collect();
        let table = Scripted::new(Snap::Readable, &rows);
        let mut next = 40;
        let kill = kill_port_owners(
            &table,
            PORT,
            move |_| {
                next += 1;
                Ok(Some(listener(next - 1)))
            },
            SELF,
            BUDGET,
        );
        assert_eq!(kill.owners, vec![40, 41, 42, 43]);
        assert_eq!(kill.owners.len(), PORT_KILL_ROUNDS);
        assert_eq!(kill.stopped_by, None);
    }

    #[test]
    fn health_port_kill_loop_rereads_an_owner_that_is_still_tearing_down() {
        // The listener still names owner 10 for one read after its kill
        // (TerminateProcess returns before the process is gone); the re-read
        // then sees the second (IPv6) owner, 30, and the loop ends it too.
        let mut rows = tree();
        rows.push((30, 5, ft(T)));
        let table = Scripted::new(Snap::Readable, &rows);
        let kill = kill_port_owners(
            &table,
            PORT,
            reads(vec![
                Ok(Some(listener(10))),
                Ok(Some(listener(10))),
                Ok(Some(listener(30))),
                Ok(None),
            ]),
            SELF,
            BUDGET,
        );
        assert_eq!(kill.owners, vec![10, 30]);
        assert_eq!(kill.killed, vec![12, 11, 10, 30]);
        assert_eq!(kill.stopped_by, Some(KillRefusal::NoListener));
    }

    // ── free_port_after_stop (ManagedProcess::stop's port sequence) ─────────

    /// Drive the sequence with scripted waits; returns (freed, call log).
    async fn stop_sequence(waits: Vec<bool>, tracked: &[PidWithSpawnedAt]) -> (bool, Vec<String>) {
        let log = RefCell::new(Vec::new());
        let waits = RefCell::new(waits.into_iter());
        let freed = free_port_after_stop(
            "svc",
            PORT,
            tracked,
            |p| {
                log.borrow_mut().push(format!("wait {p}"));
                std::future::ready(waits.borrow_mut().next().unwrap_or(false))
            },
            |root: PidWithSpawnedAt| {
                log.borrow_mut().push(format!("reap {}", root.pid));
                std::future::ready(1usize)
            },
            |p| {
                log.borrow_mut().push(format!("kill {p}"));
                std::future::ready(true)
            },
        )
        .await;
        (freed, log.into_inner())
    }

    #[tokio::test]
    async fn health_stop_frees_a_held_port_by_reaping_tracked_descendants_then_killing_then_waiting(
    ) {
        let tracked = [
            record(T),
            PidWithSpawnedAt {
                pid: 11,
                spawned_at_unix: T + 1,
                spawned_at_ft: Some(ft(T + 1)),
            },
        ];
        let (freed, log) = stop_sequence(vec![false, true], &tracked).await;
        assert!(freed);
        assert_eq!(
            log,
            vec![
                "wait 41234",
                "reap 10",
                "reap 11",
                "kill 41234",
                "wait 41234"
            ]
        );
        // Still held after all of it: reported, not hidden.
        let (freed, log) = stop_sequence(vec![false, false], &tracked).await;
        assert!(!freed);
        assert_eq!(log.len(), 5, "{log:?}");
    }

    #[tokio::test]
    async fn health_stop_does_nothing_more_when_the_port_frees() {
        let (freed, log) = stop_sequence(vec![true], &[record(T)]).await;
        assert!(freed);
        assert_eq!(log, vec!["wait 41234"]);
    }
}
