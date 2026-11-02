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
use tracing::{info, warn};

/// Env override for the parent cgroup directory (absolute path of a delegated,
/// writable cgroup v2 directory). Used by operators and by tests.
pub(crate) const CGROUP_ROOT_ENV: &str = "QONTINUI_CI_CGROUP_ROOT";

/// Where cgroup v2 is mounted on every mainstream distro.
const CGROUP_MOUNT: &str = "/sys/fs/cgroup";

/// Longest sanitized dispatch id used in a cgroup directory name.
const MAX_ID_LEN: usize = 64;

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
    format!("dispatch-{id}-{h:08x}")
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
    /// fails: an un-adopted child runs uncontained rather than not at all.
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

    /// `oom_kill` events not yet reported, as a log line; `None` when there
    /// are none (or `memory.events` is unreadable).
    pub(crate) fn take_oom_note(&self) -> Option<String> {
        let n = parse_oom_kill(&fs::read_to_string(self.dir.join("memory.events")).ok()?)?;
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
    /// leftover empty directory is reused or removed by a later run.
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
    /// No usable cgroup; the dispatch runs unbounded. Carries why.
    Uncontained(String),
}

impl Containment {
    pub(crate) fn create(dispatch_id: &str, limit_bytes: u64) -> Self {
        match CgroupDispatch::discover_and_create(dispatch_id, limit_bytes) {
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
                Containment::Uncontained(reason)
            }
        }
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
                format!("[ci-node] containment: UNCONTAINED (no memory backstop): {why}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture parent that behaves like a delegated cgroup for the file
    /// reads/writes `create_in` does (plain files stand in for cgroupfs).
    fn fixture_parent(controllers: &str, subtree: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("cgroup.controllers"), controllers).unwrap();
        fs::write(d.path().join("cgroup.subtree_control"), subtree).unwrap();
        d
    }

    #[test]
    fn dir_name_is_prefixed_and_path_safe() {
        assert!(dispatch_dir_name("abc-123").starts_with("dispatch-abc-123-"));
        assert!(dispatch_dir_name("../../etc").starts_with("dispatch-______etc-"));
        assert!(dispatch_dir_name("").starts_with("dispatch-x-"));
        assert_eq!(
            dispatch_dir_name(&"a".repeat(200)).len(),
            "dispatch-".len() + 64 + 9
        );
        assert!(!dispatch_dir_name("a/b").contains('/'));
        assert!(!dispatch_dir_name("..").contains(".."));
        // Ids that sanitize alike still get distinct directories.
        assert_ne!(dispatch_dir_name("a/b"), dispatch_dir_name("a?b"));
        assert_ne!(dispatch_dir_name("\u{e9}"), dispatch_dir_name("\u{fc}"));
    }

    #[test]
    fn unified_cgroup_is_parsed_from_proc() {
        assert_eq!(
            parse_unified_cgroup("0::/system.slice/qontinui-runner.service/runner\n").as_deref(),
            Some("/system.slice/qontinui-runner.service/runner")
        );
        assert_eq!(
            parse_unified_cgroup("0::/a/b (deleted)\n").as_deref(),
            Some("/a/b")
        );
        // cgroup v1 only: no `0::` line, so no containment.
        assert_eq!(parse_unified_cgroup("12:memory:/foo\n11:cpu:/foo\n"), None);
    }

    #[test]
    fn service_parent_is_a_candidate_only_for_service_units() {
        assert_eq!(
            candidate_parents("/system.slice/qontinui-runner.service/runner"),
            vec![
                "/system.slice/qontinui-runner.service/runner".to_string(),
                "/system.slice/qontinui-runner.service".to_string()
            ]
        );
        // Never reach up into a shared slice.
        assert_eq!(
            candidate_parents("/system.slice/qontinui-runner.service"),
            vec!["/system.slice/qontinui-runner.service".to_string()]
        );
        assert_eq!(candidate_parents("/"), vec!["/".to_string()]);
        assert_eq!(candidate_parents(""), vec!["/".to_string()]);
    }

    #[test]
    fn cgroup_dir_joins_under_the_mount() {
        assert_eq!(
            cgroup_dir(Path::new("/sys/fs/cgroup"), "/a/b"),
            PathBuf::from("/sys/fs/cgroup/a/b")
        );
        assert_eq!(
            cgroup_dir(Path::new("/sys/fs/cgroup"), "/"),
            PathBuf::from("/sys/fs/cgroup/")
        );
    }

    #[test]
    fn controller_and_event_parsers() {
        assert!(lists_memory("cpu io memory pids\n"));
        assert!(!lists_memory("cpu io pids\n"));
        assert!(!lists_memory("memory_recursiveprot\n"));
        assert_eq!(parse_procs("12\n34\nx\n\n"), vec![12, 34]);
        assert_eq!(
            parse_oom_kill("low 0\nhigh 0\nmax 3\noom 2\noom_kill 2\noom_group_kill 0\n"),
            Some(2)
        );
        assert_eq!(parse_oom_kill("low 0\n"), None);
    }

    #[test]
    fn create_writes_the_limit_the_windows_arm_uses() {
        let p = fixture_parent("cpu memory pids", "memory");
        let limit = super::super::executor::CI_JOB_MEMORY_LIMIT_BYTES;
        let c = CgroupDispatch::create_in(p.path(), "d1", limit).unwrap();
        assert_eq!(c.dir(), p.path().join(dispatch_dir_name("d1")));
        let written = fs::read_to_string(c.dir().join("memory.max")).unwrap();
        assert_eq!(written, (32u64 * 1024 * 1024 * 1024).to_string());
        assert_eq!(
            fs::read_to_string(c.dir().join("memory.oom.group")).unwrap(),
            "1"
        );
        // Fixture dirs hold plain files, so Drop's rmdir cannot succeed; the
        // TempDir owns the cleanup.
    }

    #[test]
    fn missing_memory_controller_degrades_with_a_reason() {
        let p = fixture_parent("cpu pids", "");
        let e = CgroupDispatch::create_in(p.path(), "d2", 1 << 20).unwrap_err();
        assert!(e.contains("memory controller is not available"), "{e}");
        assert!(
            !p.path().join(dispatch_dir_name("d2")).exists(),
            "nothing left behind"
        );
    }

    #[test]
    fn unreadable_parent_degrades_with_a_reason() {
        let d = tempfile::tempdir().unwrap();
        let e = CgroupDispatch::create_in(&d.path().join("nope"), "d3", 1 << 20).unwrap_err();
        assert!(e.contains("cgroup.controllers unreadable"), "{e}");
    }

    #[test]
    fn memory_is_enabled_for_children_when_listed_but_not_enabled() {
        let p = fixture_parent("memory", "");
        let _c = CgroupDispatch::create_in(p.path(), "d4", 1 << 20).unwrap();
        assert_eq!(
            fs::read_to_string(p.path().join("cgroup.subtree_control")).unwrap(),
            "+memory"
        );
    }

    #[test]
    fn kill_prefers_cgroup_kill_and_falls_back_to_the_procs_loop() {
        let p = fixture_parent("memory", "memory");
        let c = CgroupDispatch::create_in(p.path(), "d5", 1 << 20).unwrap();
        // Writable cgroup.kill: the atomic path.
        assert_eq!(c.kill(), KillPath::CgroupKill);
        assert_eq!(
            fs::read_to_string(c.dir().join("cgroup.kill")).unwrap(),
            "1"
        );
        // cgroup.kill not writable (kernel < 5.14): a directory there makes
        // the write fail; an empty cgroup.procs ends the loop at once.
        fs::remove_file(c.dir().join("cgroup.kill")).unwrap();
        fs::create_dir(c.dir().join("cgroup.kill")).unwrap();
        fs::write(c.dir().join("cgroup.procs"), "").unwrap();
        assert_eq!(c.kill(), KillPath::ProcsLoop);
    }

    #[test]
    fn oom_note_reports_each_kill_once() {
        let p = fixture_parent("memory", "memory");
        let c = CgroupDispatch::create_in(p.path(), "d6", 4096).unwrap();
        assert_eq!(
            c.take_oom_note(),
            None,
            "no memory.events yet is not a kill"
        );
        fs::write(c.dir().join("memory.events"), "max 5\noom_kill 0\n").unwrap();
        assert_eq!(c.take_oom_note(), None);
        fs::write(c.dir().join("memory.events"), "max 9\noom_kill 1\n").unwrap();
        let note = c.take_oom_note().expect("a new oom_kill is reported");
        assert!(
            note.contains("memory limit") && note.contains("4096"),
            "{note}"
        );
        assert_eq!(c.take_oom_note(), None, "reported once");
    }

    #[test]
    fn the_env_override_is_honoured_and_describe_names_the_state() {
        // Fixture root via the override. The shared env lock excludes every
        // other env-touching test, including the real-kernel cgroup test that
        // reads the variable.
        let _g = crate::test_env::env_lock();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                std::env::remove_var(CGROUP_ROOT_ENV);
            }
        }
        let p = fixture_parent("memory", "memory");
        std::env::set_var(CGROUP_ROOT_ENV, p.path());
        let _restore = Restore;
        let c = Containment::create("d7", 1 << 20);
        drop(_restore);
        assert!(matches!(c, Containment::Cgroup(_)));
        assert!(
            c.describe().contains("memory.max=1048576"),
            "{}",
            c.describe()
        );
        let u = Containment::Uncontained("no delegation".into());
        assert!(u.describe().contains("UNCONTAINED"));
        assert!(u.describe().contains("no delegation"));
    }

    /// Real-kernel check: a process exceeding `memory.max` is killed by the
    /// kernel, reported as a non-success exit (the executor maps that to a
    /// FAILURE), and does not hang. Skips with a message when the box offers
    /// no delegated writable cgroup v2 subtree (CI containers, dev laptops).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_dispatch_exceeding_memory_max_is_killed_and_fails() {
        use std::os::unix::process::ExitStatusExt;
        let limit: u64 = 64 * 1024 * 1024;
        // The lock covers only the env read inside discovery (never an await).
        let cg = {
            let _g = crate::test_env::env_lock();
            match CgroupDispatch::discover_and_create(
                &format!("memtest-{}", std::process::id()),
                limit,
            ) {
                Ok(c) => c,
                Err(why) => {
                    eprintln!("SKIPPED: no delegated writable cgroup v2 subtree here: {why}");
                    return;
                }
            }
        };
        // Gated start: the hog (`tail -n 1 /dev/zero` buffers an endless
        // "line") only begins after placement is verified, so a placement
        // failure can never leave an unbounded hog running on the host.
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "read x; exec tail -n 1 /dev/zero"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cg.prepare(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sh");
        cg.adopt(&child);
        let pid = child.id().expect("pid");
        let placed = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        let want = cg.dir().file_name().unwrap().to_string_lossy().into_owned();
        if !placed.contains(&want) {
            let _ = child.start_kill();
            let _ = child.wait().await;
            panic!("child not placed in {want}: {placed:?}");
        }
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().expect("stdin");
            stdin.write_all(b"go\n").await.expect("release");
        }
        let status = tokio::time::timeout(std::time::Duration::from_secs(60), child.wait())
            .await
            .expect("an over-limit dispatch must be killed, not hang")
            .expect("wait");
        assert!(
            !status.success(),
            "over-limit dispatch must not succeed: {status:?}"
        );
        assert_eq!(
            status.signal(),
            Some(9),
            "killed by SIGKILL (OOM): {status:?}"
        );
        assert!(
            cg.take_oom_note().is_some(),
            "memory.events should record the oom_kill"
        );
        // Drop kills strays and rmdirs; verify the directory is gone.
        let dir = cg.dir().to_path_buf();
        drop(cg);
        for _ in 0..80 {
            if !dir.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(!dir.exists(), "cgroup directory removed after the dispatch");
    }
}
