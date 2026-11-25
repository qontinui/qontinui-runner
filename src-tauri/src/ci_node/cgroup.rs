//! Linux host-side containment for one CI dispatch (plan
//! `2026-10-19-ci-nodes-run-the-tauri-runner-always-on-as-ci-node-role`, D3).
//!
//! The Windows arm bounds a dispatch with a per-dispatch Job Object carrying
//! `CI_JOB_MEMORY_LIMIT_BYTES`. The Linux equivalent is a cgroup v2 child:
//! with a delegated subtree (systemd `Delegate=yes`) the runner creates
//! `dispatch-<id>` beneath it, writes `memory.max`, places the step process
//! tree inside, and kills the subtree on cancel / lease loss / timeout /
//! dispatch end, then `rmdir`s it.
//!
//! ## Degradation is explicit, never silent
//!
//! No delegated, writable cgroup v2 subtree (not Linux, cgroup v1, no
//! delegation, no `memory` controller, a read-only mount, a container) yields
//! [`Containment::Uncontained`] carrying the reason. The dispatch still runs —
//! failing a build merely because the box cannot contain it would turn a
//! backstop into an outage — but the state is logged and pushed to the
//! dispatch log by the executor ([`Containment::describe`]).
//!
//! A dispatch cgroup that a step cannot be moved into degrades the same way,
//! one step at a time. The kernel moves a process only for a writer that may
//! write `cgroup.procs` in the nearest cgroup holding both the one the process
//! leaves and the one it enters (`cgroup_procs_write_permission`), so a
//! `QONTINUI_CI_CGROUP_ROOT` delegated to the runner's user outside its user
//! manager (under `ci.slice`, say) passes every check
//! [`CgroupDispatch::create_in`] makes and still has every move refused. Such a
//! step runs in the runner's own cgroup: [`CgroupDispatch::adopt`] warns, and
//! the dispatch counts as running uncontained ([`dispatch_counts`]) from then
//! until it ends.
//!
//! `ci_node`'s own work for a dispatch runs in the runner's own cgroup as well,
//! whatever the job's containment: the checkout, the canonical-configuration
//! convergence and the provisioning before [`Containment::create`] settles it,
//! and the report and the cleanup after the job's steps. [`OutsideCgroup`]
//! counts that work as an uncontained run too. The runner's handling of the
//! steps while they run (reading their output and posting it) is not counted:
//! that would count every dispatch as uncontained from start to end.
//!
//! ## Layout requirement (the kernel's "no internal processes" rule)
//!
//! A cgroup cannot both hold processes and enable controllers for its
//! children. The runner's own cgroup holds the runner, so `memory` is only
//! enableable there if the unit uses `DelegateSubgroup=` (systemd >= 254),
//! which parks the main process in a sub-cgroup and leaves the unit cgroup
//! process-free. [`candidate_parents`] therefore tries the runner's own
//! cgroup first and, if that cgroup's parent is a `*.service` unit cgroup,
//! that parent second. `QONTINUI_CI_CGROUP_ROOT` overrides discovery.
//!
//! ## Relation to `ci-exec`
//!
//! `qontinui-schemas` `ci-exec` stays platform-agnostic; its
//! `StepContainment::adopt(&self, &Child)` seam (host.rs) is implemented host
//! side. [`CgroupDispatch::adopt`] has exactly that shape, so once the
//! executor moves behind `ProcessSpawn` (runner#1995) the impl is a thin
//! wrapper over this module.

// The cgroup filesystem is Linux-only, but the path/limit logic and the
// fixture-directory tests are plain `std::fs` and run on every dev OS.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use tracing::{info, warn};

/// Env override for the parent cgroup directory (absolute path of a delegated,
/// writable cgroup v2 directory). Used by operators and by tests.
pub(crate) const CGROUP_ROOT_ENV: &str = "QONTINUI_CI_CGROUP_ROOT";

/// Where cgroup v2 is mounted on every mainstream distro.
const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

/// Longest sanitized dispatch id used in a cgroup directory name.
const MAX_ID_LEN: usize = 64;

/// How every dispatch cgroup's directory name starts ([`dispatch_dir_name`]).
/// The slice telemetry finds the runner's dispatch cgroups by it, and charges
/// their CPU to CI rather than to the runner's unit.
pub(crate) const DISPATCH_DIR_PREFIX: &str = "dispatch-";

/// Dispatches whose containment [`Containment::create`] settled, contained or
/// not ([`dispatch_counts`]).
static DISPATCHES_STARTED: AtomicU64 = AtomicU64::new(0);
/// Uncontained runs alive now: one per live [`UncontainedRun`].
static UNCONTAINED_RUNNING: AtomicU64 = AtomicU64::new(0);
/// Uncontained runs begun so far: one per [`UncontainedRun`] made. Never falls.
static UNCONTAINED_BEGUN: AtomicU64 = AtomicU64::new(0);

/// What this process's CI dispatches had done at one moment. The slice
/// telemetry compares two of these to learn that a dispatch ran between two
/// of its reads without either read listing its cgroup: one that started and
/// ended in between, or one whose steps ran uncontained in the runner's own
/// cgroup.
///
/// The counts are THIS process's only: the statics live in this runner's
/// address space. CI work another runner process runs moves none of them, so
/// it never withholds the split (`dispatch_started`, `dispatch_uncontained`).
/// A dispatch cgroup of its that a read lists is still charged to CI; its
/// uncontained work in this runner's unit, and a dispatch cgroup of its that
/// no read listed, are charged to session work with nothing withheld.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DispatchCounts {
    /// Dispatches whose containment was settled. Each is counted once its
    /// cgroup exists, or could not be made, so a reader that takes this before
    /// it lists the cgroups either lists a new dispatch or sees this move. The
    /// executor starts a dispatch's steps only after that, so a reader that
    /// takes this again after reading the cgroups' counters sees any dispatch
    /// whose steps those counters hold time of. Where the count is moved
    /// ([`Containment::create`]) is argued here, not exercised: no test can
    /// read between the cgroup's creation and the count.
    pub(crate) started: u64,
    /// Uncontained runs alive now ([`UncontainedRun`]): a dispatch running
    /// uncontained, a contained one since a step of it could not be placed in
    /// its cgroup, and `ci_node`'s own work for a dispatch ([`OutsideCgroup`]).
    pub(crate) uncontained_running: u64,
    /// Uncontained runs begun so far. A step that cannot be placed can begin
    /// one after a read and its dispatch can end before the next, with
    /// `started` unmoved, so only this count shows that one ran in between.
    pub(crate) uncontained_begun: u64,
}

/// This process's [`DispatchCounts`] now. `uncontained_begun` is loaded before
/// `uncontained_running`, the reverse of the order [`UncontainedRun`] moves
/// them in, so counts that hold a run's start also hold it running, if it
/// still is. Both orders are argued here, not exercised: no test can begin or
/// end a run between two of these loads.
pub(crate) fn dispatch_counts() -> DispatchCounts {
    let started = DISPATCHES_STARTED.load(Ordering::SeqCst);
    let uncontained_begun = UNCONTAINED_BEGUN.load(Ordering::SeqCst);
    let uncontained_running = UNCONTAINED_RUNNING.load(Ordering::SeqCst);
    DispatchCounts {
        started,
        uncontained_running,
        uncontained_begun,
    }
}

/// Directory name for a dispatch's cgroup: `dispatch-<sanitized id>-<hash>`.
/// Only `[A-Za-z0-9_-]` survive (a `/` or `..` must never reach a path join);
/// an empty result becomes `x`. The FNV-1a hash of the RAW id keeps two ids
/// that sanitize (or truncate) to the same string in different directories, so
/// one dispatch can never reuse or remove another's live cgroup.
pub(crate) fn dispatch_dir_name(dispatch_id: &str) -> String {
    let mut id: String = dispatch_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_ID_LEN)
        .collect();
    if id.is_empty() {
        id.push('x');
    }
    let mut h: u32 = 0x811c_9dc5;
    for b in dispatch_id.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    format!("{DISPATCH_DIR_PREFIX}{id}-{h:08x}")
}

/// Extract the cgroup v2 path from `/proc/<pid>/cgroup` content (the
/// `0::/some/path` line). `None` on a v1-only host (no `0::` line).
pub(crate) fn parse_unified_cgroup(proc_cgroup: &str) -> Option<String> {
    proc_cgroup.lines().find_map(|l| {
        let rest = l.strip_prefix("0::")?;
        let rest = rest.strip_suffix(" (deleted)").unwrap_or(rest);
        Some(rest.trim().to_string())
    })
}

/// Cgroup paths (relative to the mount, `/`-rooted) to try as the parent of
/// `dispatch-<id>`, best first. See the module docs for why the parent is a
/// candidate at all, and why only when it is a `*.service` unit cgroup.
pub(crate) fn candidate_parents(own: &str) -> Vec<String> {
    let own = if own.is_empty() { "/" } else { own };
    let mut out = vec![own.to_string()];
    if let Some((parent, _leaf)) = own.rsplit_once('/') {
        let parent_name = parent.rsplit('/').next().unwrap_or("");
        if parent_name.ends_with(".service") {
            out.push(parent.to_string());
        }
    }
    out
}

/// Join a `/`-rooted cgroup path onto the mount point.
pub(crate) fn cgroup_dir(mount: &Path, cgroup_path: &str) -> PathBuf {
    mount.join(cgroup_path.trim_start_matches('/'))
}

/// True when a `cgroup.controllers` / `cgroup.subtree_control` body lists
/// `memory`.
pub(crate) fn lists_memory(controllers: &str) -> bool {
    controllers.split_whitespace().any(|c| c == "memory")
}

/// Pids from a `cgroup.procs` body (one per line; junk lines skipped).
pub(crate) fn parse_procs(body: &str) -> Vec<i32> {
    body.lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .collect()
}

/// `oom_kill` counter from a `memory.events` body.
pub(crate) fn parse_oom_kill(events: &str) -> Option<u64> {
    events.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        (it.next()? == "oom_kill").then(|| it.next()?.parse().ok())?
    })
}

/// How the subtree was killed (observable in tests and logs).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum KillPath {
    /// `cgroup.kill` (kernel >= 5.14): atomic, no fork race.
    CgroupKill,
    /// `cgroup.procs` SIGKILL loop until the group is empty.
    ProcsLoop,
    /// The fallback loop gave up with processes still listed.
    Incomplete,
}

/// One dispatch's live cgroup.
#[derive(Debug)]
pub(crate) struct CgroupDispatch {
    dir: PathBuf,
    limit_bytes: u64,
    /// `oom_kill` already reported, so each step reports only new kills.
    oom_reported: AtomicU64,
    /// Made at the first step [`adopt`](Self::adopt) could not place here, and
    /// held until the dispatch drops: that step ran in the runner's own cgroup.
    stray: OnceLock<UncontainedRun>,
}

impl CgroupDispatch {
    /// Create `<parent>/dispatch-<id>` with `memory.max = limit_bytes`.
    ///
    /// `Err(reason)` leaves nothing behind (a half-made directory is removed).
    pub(crate) fn create_in(
        parent: &Path,
        dispatch_id: &str,
        limit_bytes: u64,
    ) -> Result<Self, String> {
        let controllers = fs::read_to_string(parent.join("cgroup.controllers"))
            .map_err(|e| format!("{}: cgroup.controllers unreadable: {e}", parent.display()))?;
        if !lists_memory(&controllers) {
            return Err(format!(
                "{}: the memory controller is not available (controllers: {:?})",
                parent.display(),
                controllers.trim()
            ));
        }
        let subtree = parent.join("cgroup.subtree_control");
        let enabled = fs::read_to_string(&subtree).unwrap_or_default();
        if !lists_memory(&enabled) {
            fs::write(&subtree, "+memory").map_err(|e| {
                format!(
                    "{}: cannot enable the memory controller for children: {e} \
                     (EBUSY means this cgroup holds processes: run the unit with \
                     Delegate=yes and DelegateSubgroup=, or set {CGROUP_ROOT_ENV})",
                    parent.display()
                )
            })?;
        }

        let dir = parent.join(dispatch_dir_name(dispatch_id));
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A leftover from a crashed runner: usable only if empty (rmdir
                // refuses otherwise, which is the safe answer).
                fs::remove_dir(&dir)
                    .map_err(|e2| format!("{}: stale and not removable: {e2}", dir.display()))?;
                fs::create_dir(&dir).map_err(|e2| format!("{}: mkdir: {e2}", dir.display()))?;
            }
            Err(e) => return Err(format!("{}: mkdir: {e}", dir.display())),
        }

        let this = Self {
            dir,
            limit_bytes,
            oom_reported: AtomicU64::new(0),
            stray: OnceLock::new(),
        };
        if let Err(e) = fs::write(this.dir.join("memory.max"), limit_bytes.to_string()) {
            let reason = format!("{}: memory.max write: {e}", this.dir.display());
            let _ = fs::remove_dir(&this.dir);
            return Err(reason);
        }
        // Best effort: a memory kill takes the whole group (a half-killed
        // cargo tree is worse than a dead one), and swap must not turn the
        // backstop into a slow death. Absent files (no swap accounting) are fine.
        let _ = fs::write(this.dir.join("memory.oom.group"), "1");
        let _ = fs::write(this.dir.join("memory.swap.max"), "0");
        Ok(this)
    }

    /// Discover a delegated parent (env override, then the runner's own
    /// cgroup and its service parent) and create the dispatch cgroup there.
    pub(crate) fn discover_and_create(dispatch_id: &str, limit_bytes: u64) -> Result<Self, String> {
        if let Some(root) = std::env::var_os(CGROUP_ROOT_ENV).filter(|v| !v.is_empty()) {
            return Self::create_in(Path::new(&root), dispatch_id, limit_bytes);
        }
        if !cfg!(target_os = "linux") {
            return Err("not Linux: cgroup containment does not apply".into());
        }
        let mount = Path::new(CGROUP_MOUNT);
        if !mount.join("cgroup.controllers").exists() {
            return Err(format!(
                "{CGROUP_MOUNT} is not a cgroup v2 mount (cgroup v1 or no cgroupfs)"
            ));
        }
        let own = parse_unified_cgroup(
            &fs::read_to_string("/proc/self/cgroup")
                .map_err(|e| format!("/proc/self/cgroup unreadable: {e}"))?,
        )
        .ok_or_else(|| "no cgroup v2 entry in /proc/self/cgroup".to_string())?;
        let mut reasons = Vec::new();
        for cand in candidate_parents(&own) {
            if cand == "/" {
                reasons.push(format!(
                    "runner is in the root cgroup; refusing to enable controllers there \
                     (set {CGROUP_ROOT_ENV} to opt in)"
                ));
                continue;
            }
            match Self::create_in(&cgroup_dir(mount, &cand), dispatch_id, limit_bytes) {
                Ok(c) => return Ok(c),
                Err(e) => reasons.push(e),
            }
        }
        Err(reasons.join("; "))
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn limit_bytes(&self) -> u64 {
        self.limit_bytes
    }

    /// Make `cmd` start inside this cgroup: the forked child writes `0`
    /// ("myself") to `cgroup.procs` before `exec`, so there is no window in
    /// which it (or anything it forks) runs outside the limit. Failure is
    /// ignored here; [`adopt`](Self::adopt) re-checks after spawn.
    #[cfg(unix)]
    pub(crate) fn prepare(&self, cmd: &mut tokio::process::Command) {
        use std::os::fd::AsRawFd;
        let Ok(procs) = fs::OpenOptions::new()
            .write(true)
            .open(self.dir.join("cgroup.procs"))
        else {
            warn!(
                "ci_node: cannot open {}/cgroup.procs - step starts outside the cgroup \
                 until adopted",
                self.dir.display()
            );
            return;
        };
        // SAFETY: the closure runs between fork and exec and calls only the
        // async-signal-safe `write(2)` on an fd opened before the fork (the
        // `File` moved in keeps it open; it is O_CLOEXEC, so it does not leak
        // into the step).
        unsafe {
            cmd.pre_exec(move || {
                let fd = procs.as_raw_fd();
                let _ = libc::write(fd, b"0\n".as_ptr().cast(), 2);
                Ok(())
            });
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn prepare(&self, _cmd: &mut tokio::process::Command) {}

    /// Bring a spawned child under the cgroup if [`prepare`](Self::prepare)
    /// did not (shape of `ci-exec`'s `StepContainment::adopt`). Warns, never
    /// fails: an un-adopted child runs uncontained rather than not at all, and
    /// from the first one the dispatch counts as running uncontained
    /// ([`dispatch_counts`]) until it drops.
    pub(crate) fn adopt(&self, child: &tokio::process::Child) {
        let Some(pid) = child.id() else { return };
        let want = self
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let already = fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .ok()
            .and_then(|b| parse_unified_cgroup(&b))
            .map(|p| p.rsplit('/').next().map(str::to_string) == want)
            .unwrap_or(false);
        if already {
            return;
        }
        if let Err(e) = fs::write(self.dir.join("cgroup.procs"), pid.to_string()) {
            warn!(
                "ci_node: could not place step pid {pid} in {}: {e} — step runs UNCONTAINED",
                self.dir.display()
            );
            self.stray.get_or_init(|| {
                UncontainedRun::new(format!(
                    "step pid {pid} could not be placed in {}: {e}",
                    self.dir.display()
                ))
            });
        }
    }

    /// Kill every process in the subtree. Idempotent.
    pub(crate) fn kill(&self) -> KillPath {
        if fs::write(self.dir.join("cgroup.kill"), "1").is_ok() {
            return KillPath::CgroupKill;
        }
        // No cgroup.kill (kernel < 5.14): SIGKILL until empty. Re-reading the
        // list each pass picks up children forked between passes.
        // No sleeping: this runs on async workers, so passes only yield.
        for _ in 0..200 {
            let pids = fs::read_to_string(self.dir.join("cgroup.procs"))
                .map(|b| parse_procs(&b))
                .unwrap_or_default();
            if pids.is_empty() {
                return KillPath::ProcsLoop;
            }
            for pid in pids {
                kill_pid(pid);
            }
            std::thread::yield_now();
        }
        warn!(
            "ci_node: cgroup {} still has processes after kill",
            self.dir.display()
        );
        KillPath::Incomplete
    }

    /// The kernel's cumulative `oom_kill` count for this dispatch's cgroup;
    /// `None` when `memory.events` is unreadable (UNKNOWN, never 0).
    pub(crate) fn oom_kill_count(&self) -> Option<u64> {
        parse_oom_kill(&fs::read_to_string(self.dir.join("memory.events")).ok()?)
    }

    /// `oom_kill` events not yet reported, as a log line; `None` when there
    /// are none (or `memory.events` is unreadable).
    pub(crate) fn take_oom_note(&self) -> Option<String> {
        let n = self.oom_kill_count()?;
        let prev = self.oom_reported.swap(n, Ordering::Relaxed);
        (n > prev).then(|| {
            format!(
                "[ci-node] killed by the kernel: dispatch exceeded its cgroup memory limit \
                 ({} bytes, {n} oom_kill event(s))",
                self.limit_bytes
            )
        })
    }
}

#[cfg(unix)]
fn kill_pid(pid: i32) {
    // SAFETY: plain kill(2); a stale pid at worst ESRCHes.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_pid(_pid: i32) {}

impl Drop for CgroupDispatch {
    /// Kill strays, then `rmdir`. The kernel keeps a cgroup until its last
    /// process is reaped, so a first `rmdir` can fail EBUSY; the retries run
    /// on a detached thread so this (async-context) drop never sleeps. A
    /// leftover empty directory is removed only if the SAME dispatch id runs
    /// again (`create_in`); otherwise it stays until the unit's cgroup is
    /// torn down (systemd removes a delegated subtree when the unit stops).
    /// Under a `QONTINUI_CI_CGROUP_ROOT` systemd does not manage, nothing
    /// removes it: dispatch ids are not reused.
    fn drop(&mut self) {
        self.kill();
        match fs::remove_dir(&self.dir) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => {}
        }
        let dir = self.dir.clone();
        let _ = std::thread::Builder::new()
            .name("ci-cgroup-rmdir".into())
            .spawn(move || {
                for _ in 0..80 {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                    match fs::remove_dir(&dir) {
                        Ok(()) => return,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                        Err(_) => {}
                    }
                }
                warn!("ci_node: could not remove cgroup {}", dir.display());
            });
    }
}

/// The containment state of one dispatch.
#[derive(Debug)]
pub(crate) enum Containment {
    Cgroup(CgroupDispatch),
    /// No usable cgroup; the dispatch runs unbounded, in the runner's own
    /// cgroup. Carries why.
    Uncontained(UncontainedRun),
}

/// Why a dispatch runs uncontained, wholly or from one of its steps on. It is
/// counted in [`dispatch_counts`] for as long as it lives: as long as an
/// uncontained dispatch holds its containment, for a contained one from its
/// first step that could not be placed until it drops, and while
/// [`OutsideCgroup`] counts `ci_node`'s own work for a dispatch.
#[derive(Debug)]
pub(crate) struct UncontainedRun {
    reason: String,
}

impl UncontainedRun {
    fn new(reason: String) -> Self {
        // Running before begun: see `dispatch_counts`.
        UNCONTAINED_RUNNING.fetch_add(1, Ordering::SeqCst);
        UNCONTAINED_BEGUN.fetch_add(1, Ordering::SeqCst);
        Self { reason }
    }
}

impl Drop for UncontainedRun {
    fn drop(&mut self) {
        UNCONTAINED_RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Kept for a test that names its reason as a literal. It makes a run,
/// counted like any other, so production code calls [`UncontainedRun::new`],
/// where that is plain to see.
impl From<&str> for UncontainedRun {
    fn from(reason: &str) -> Self {
        Self::new(reason.to_string())
    }
}

/// `ci_node`'s own work for one dispatch, counted as an uncontained run. It
/// runs in the runner's own cgroup, beside session work, whatever the job's
/// containment: the checkout, the canonical-configuration convergence and the
/// provisioning before [`Containment::create`] settles the containment (and so
/// all of a dispatch refused or failed before then), and the report and the
/// cleanup after the job's steps. `executor::run_dispatch` makes one before
/// the checkout and holds it until it returns.
#[derive(Debug)]
pub(crate) struct OutsideCgroup {
    dispatch_id: String,
    run: Option<UncontainedRun>,
}

impl OutsideCgroup {
    /// Count the dispatch's setup, from now until [`OutsideCgroup::settled`].
    pub(crate) fn setup(dispatch_id: &str) -> Self {
        Self {
            dispatch_id: dispatch_id.to_string(),
            run: Some(UncontainedRun::new(format!(
                "ci_node sets up dispatch {dispatch_id} in the runner's own cgroup"
            ))),
        }
    }

    /// The job's containment is settled, and counts the job from here: an
    /// uncontained job by its own run, which began before this one ends, and a
    /// contained one by its cgroup and by any step that could not be placed in
    /// it. So the setup's run ends.
    pub(crate) fn settled(&mut self) {
        self.run = None;
    }

    /// The job's steps are over: count what `ci_node` runs from here until
    /// this drops. One run at a time, so a setup still counted (no containment
    /// was settled) goes on counting.
    pub(crate) fn after_steps(&mut self) {
        if self.run.is_none() {
            self.run = Some(UncontainedRun::new(format!(
                "ci_node winds down dispatch {} in the runner's own cgroup",
                self.dispatch_id
            )));
        }
    }
}

impl Containment {
    pub(crate) fn create(dispatch_id: &str, limit_bytes: u64) -> Self {
        let containment = match CgroupDispatch::discover_and_create(dispatch_id, limit_bytes) {
            Ok(c) => {
                info!(
                    "ci_node: dispatch {dispatch_id} contained in {} (memory.max={limit_bytes})",
                    c.dir().display()
                );
                Containment::Cgroup(c)
            }
            Err(reason) => {
                warn!(
                    "ci_node: dispatch {dispatch_id} runs UNCONTAINED (no cgroup memory \
                     backstop): {reason}"
                );
                Containment::Uncontained(UncontainedRun::new(reason))
            }
        };
        // Only now that the cgroup exists: see `DispatchCounts::started`.
        DISPATCHES_STARTED.fetch_add(1, Ordering::SeqCst);
        containment
    }

    /// One-line state for the dispatch log, so the operator sees it per dispatch.
    pub(crate) fn describe(&self) -> String {
        match self {
            Containment::Cgroup(c) => format!(
                "[ci-node] containment: cgroup {} memory.max={}",
                c.dir().display(),
                c.limit_bytes()
            ),
            Containment::Uncontained(why) => {
                format!(
                    "[ci-node] containment: UNCONTAINED (no memory backstop): {}",
                    why.reason
                )
            }
        }
    }
}

#[cfg(test)]
mod tests;
