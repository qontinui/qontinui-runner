//! Shared cargo target-dir lock state, published on the 60 s volumes-only
//! census tick as `cargo_locks`.
//!
//! Plan `2026-09-19-build-slots-are-not-build-parallelism-cargo-target-lock-and-devops-allocation-stats`,
//! Phase 2. A coord build slot is an ADMISSION count; what actually decides
//! whether a build runs is cargo's exclusive `flock` on `<target>/<profile>/.cargo-lock`.
//! Measured 2026-09-29: coord's `target/debug/.cargo-lock` had one holder and
//! 17 blocked waiters, and nothing in the fleet's telemetry could see it. This
//! module makes that queue observable per machine.
//!
//! ## What is measured
//!
//! Every `.cargo-lock` file of every SHARED target of the primary checkouts —
//! the same canonical `qontinui-*` repo roots the census walks
//! ([`super::census::is_canonical_repo_root`] under
//! [`super::census::qontinui_root`]). Per repo root (and its `src-tauri/`
//! sub-root, where the runner's own cargo workspace lives) the candidate
//! target dirs are `target`, `target-agent` and each `target-pool/*` slot; the
//! lock files are `<t>/*/.cargo-lock`, and `<t>/*/*/.cargo-lock` only under a
//! level-1 dir that has no lock of its own (the `--target <triple>` layout).
//! No deeper recursion, and at most [`MAX_CARGO_LOCK_ITEMS`] items.
//!
//! Idle locks are reported too (holder fields `null`, `waiters: 0`): the item
//! set is the census of shared targets, not only the busy ones.
//!
//! ## How
//!
//! Linux only. `/proc/locks` is read ONCE per tick and parsed by the pure
//! [`parse_proc_locks`] into `(major, minor, inode) → holders + waiters`; a
//! plain row is a granted lock, a `->` row is a process BLOCKED on that same
//! lock. Lock files are matched by `st_dev` / `st_ino` (with an inode-only
//! fallback, see [`lookup_lock`]).
//!
//! ### The pid in `/proc/locks` is the lock's TAKER, not its owner
//!
//! A flock belongs to an open file description, and `/proc/locks` prints the
//! pid of the process that CALLED `flock(2)`. When that is a short-lived
//! helper on an inherited fd — the claude-config sweeper does
//! `exec {fd}<file; flock -x "$fd"`, so `flock(1)` exits and the bash that
//! holds the fd owns the lock — the printed pid is dead, and after pid reuse it
//! names an unrelated process. So a printed pid is published only when that
//! process is CONFIRMED to hold the lock (a `lock:` line on the same file in
//! its `/proc/<pid>/fdinfo/*`). Otherwise the owner is resolved by the
//! processes that hold the lock file open, confirmed the same way (fdinfo
//! lists only a GRANTED lock, so a waiter never confirms); and if nothing confirms, `holder_pid` is `null` with
//! `holder_kind: "unknown"` — never an unconfirmed pid.
//!
//! ### A held lock can have NO `/proc/locks` row at all
//!
//! When `/proc` is mounted in a pid namespace other than the kernel's initial
//! one (a WSL2 distro, a container) and the taker has exited, the kernel drops
//! the lock's whole tree from `/proc/locks`, waiters included:
//! `locks_translate_pid()` maps the freed taker pid to 0 and `locks_show()`
//! skips the row (`fs/locks.c:2149-2172`, `:2781-2790` at WSL tag
//! `linux-msft-wsl-6.6.87.2`). The holder's `/proc/<pid>/fdinfo/<fd>` still
//! carries the `lock:` line (with pid 0). So a lock file with no holder row is
//! resolved through the same path-confirmed fdinfo arm before it is reported
//! idle. Plan `2026-10-09-cargo-target-liveness-holders-reads-a-hidden-lock-as-idle`.
//!
//! **Residue, stated so nobody reads an idle item as proof:** a row-less lock
//! whose holder's fd this uid cannot read (another uid; the scan skips those up
//! front) still reports idle, as does one held through a different path to the
//! same inode (a hardlink), and one on a filesystem whose `st_dev` differs from
//! the device the kernel prints (overlayfs — the case [`lookup_lock`]'s
//! inode-only fallback exists for), since the fdinfo confirmation compares the
//! full key. The census runs no `flock` probe of its own. That
//! is a reporting gap, not a deletion risk: `cargo_locks` is display data and
//! nothing reclaims from it (qontinui-coord has no reader of it).
//!
//! **Cost:** the open-file scan (bounded by [`MAX_FD_SCAN`]) is built at most
//! once per tick, but since every IDLE lock is also a row miss it now runs on
//! essentially every tick that has a shared lock file, not only on a held one.
//!
//! The owner is named from `/proc/<pid>/cmdline` (never `environ`) and aged
//! from `/proc/<pid>/stat` `starttime` against `/proc/uptime`.
//!
//! ## Wire contract (UNKNOWN is not empty)
//!
//! [`probe_current`] answers `None` on a non-Linux host, when no workspace
//! root resolves, or when `/proc/locks` cannot be read — and the census body
//! then OMITS `cargo_locks`, which coord reads as UNKNOWN. `Some(vec![])` is a
//! measurement: the probe ran and found no shared target lock files.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Ceiling on items per tick. The fleet's workspaces hold a few dozen shared
/// targets; the bound exists so a pathological tree cannot bloat the POST.
pub const MAX_CARGO_LOCK_ITEMS: usize = 256;

/// Truncation of the holder's command line, in chars.
const HOLDER_CMD_MAX_CHARS: usize = 200;

/// How far up the parent chain the `sweep` classification looks.
const MAX_ANCESTOR_DEPTH: usize = 16;

/// Ceiling on `/proc/<pid>/fd/*` entries the open-file owner scan reads per
/// tick — it bounds the one expensive fallback.
#[cfg(target_os = "linux")]
const MAX_FD_SCAN: usize = 100_000;

/// Sub-roots of a repo checkout that may host a cargo workspace's targets.
/// `""` is the repo root itself; `src-tauri` is the runner's cargo workspace.
const TARGET_BASES: &[&str] = &["", "src-tauri"];

/// Target dirs under a base that are SHARED (not per-worktree).
const SHARED_TARGET_NAMES: &[&str] = &["target", "target-agent"];

/// Parent dir of the build-pool slots (`target-pool/slot-0`, …).
const TARGET_POOL: &str = "target-pool";

const LOCK_FILE_NAME: &str = ".cargo-lock";

/// The sweeper's script name, matched as an argv element's basename.
const SWEEP_SCRIPT: &str = "cargo-target-liveness.sh";

/// What kind of process holds a target lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HolderKind {
    /// A cargo / rustc / clippy / rustdoc process.
    Build,
    /// The claude-config sweeper (`cargo-target-liveness.sh run-locked`), or a
    /// descendant of it.
    Sweep,
    /// Anything else, a holder whose command line could not be read, or a
    /// granted lock whose owner could not be confirmed (then `holder_pid` is
    /// `null`).
    Unknown,
}

/// One `.cargo-lock` file of one shared target — the wire item of
/// `WorktreeCensusReq::cargo_locks`. Absent values serialize as `null`
/// (the contract spells every key), never omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CargoLockItem {
    /// `<repo>/<lock's parent dir relative to the repo root>`, `/`-separated,
    /// e.g. `qontinui-coord/target/debug`.
    pub target_key: String,
    /// A CONFIRMED owner pid, or `null` (idle, or held by an unconfirmable owner).
    pub holder_pid: Option<u32>,
    /// `null` iff the lock is not held.
    pub holder_kind: Option<HolderKind>,
    pub holder_age_secs: Option<u64>,
    pub holder_cmd: Option<String>,
    /// Blocked rows (`->`) in `/proc/locks` — including rows whose pid could
    /// not be translated (`0`) or is an OFD lock (`-1`).
    pub waiters: u32,
    /// Age of the oldest waiting PROCESS — an upper bound on how long a
    /// long-lived waiter (a cargo) has waited, since `/proc/locks` does not
    /// record when a wait began. It is a LOWER bound for the sweeper, whose
    /// wait is a loop of short `flock -w 5` draws, each a fresh process.
    pub oldest_wait_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// /proc/locks parsing (pure).
// ---------------------------------------------------------------------------

/// A file identity as `/proc/locks` prints it: `MAJOR:MINOR:INODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LockKey {
    pub major: u32,
    pub minor: u32,
    pub ino: u64,
}

/// Every pid granted (`holders`) and blocked on (`waiters`) one lock, in
/// `/proc/locks` order. Pids are RAW: `0` (outside this pid namespace) and
/// `-1` (an OFD lock) are kept, because the row still says the lock is held
/// or waited on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockEntry {
    pub holders: Vec<i32>,
    pub waiters: Vec<i32>,
}

/// Parse `/proc/locks` text. Rows look like
///
/// ```text
/// 3: FLOCK  ADVISORY  WRITE 1351229 08:01:16669579 0 EOF
/// 3: -> FLOCK  ADVISORY  WRITE 1655862 08:01:16669579 0 EOF
/// ```
///
/// The device is `MAJOR:MINOR` in HEX, the inode in DECIMAL. Every lock type
/// (FLOCK, POSIX, OFDLCK, …) is keyed the same way — the caller matches by
/// inode, so unrelated rows are simply never looked up. A malformed row is
/// skipped rather than guessed at.
pub fn parse_proc_locks(text: &str) -> HashMap<LockKey, LockEntry> {
    let mut map: HashMap<LockKey, LockEntry> = HashMap::new();
    for line in text.lines() {
        let Some((key, pid, waiting)) = parse_proc_locks_line(line) else {
            continue;
        };
        let entry = map.entry(key).or_default();
        if waiting {
            entry.waiters.push(pid);
        } else {
            entry.holders.push(pid);
        }
    }
    map
}

/// One row → `(key, raw pid, is_waiter)`, or `None` for anything malformed.
/// Also parses the body of an fdinfo `lock:` line, which uses the same format.
fn parse_proc_locks_line(line: &str) -> Option<(LockKey, i32, bool)> {
    let mut tokens = line.split_whitespace();
    let id = tokens.next()?;
    if !id.ends_with(':') {
        return None;
    }
    let mut rest: Vec<&str> = tokens.collect();
    let waiting = rest.first() == Some(&"->");
    if waiting {
        rest.remove(0);
    }
    // TYPE MODE ACCESS PID MAJ:MIN:INODE START END
    if rest.len() < 7 {
        return None;
    }
    let pid: i64 = rest.get(3)?.parse().ok()?;
    let pid = i32::try_from(pid).ok()?;
    let key = parse_lock_key(rest.get(4)?)?;
    Some((key, pid, waiting))
}

/// `08:01:16669579` → `LockKey { major: 8, minor: 1, ino: 16669579 }`.
fn parse_lock_key(field: &str) -> Option<LockKey> {
    let mut parts = field.split(':');
    let major = u32::from_str_radix(parts.next()?, 16).ok()?;
    let minor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let ino: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(LockKey { major, minor, ino })
}

/// How a lock file was matched to a `/proc/locks` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMatch {
    /// Same device and inode.
    Exact,
    /// Inode only — see [`lookup_lock`].
    InodeOnly,
}

/// Find the `/proc/locks` entry for a lock file.
///
/// Exact `(major, minor, inode)` first. FALLBACK, hypothesis-driven and
/// unmeasured here (this box is ext4): on btrfs subvolumes and overlayfs the
/// `st_dev` that `stat` reports is an ANONYMOUS device (major `0`) that can
/// differ from the superblock device `/proc/locks` prints, so an exact miss
/// would read a held lock as idle. Only for such a file (`key.major == 0`),
/// when the exact key misses but the inode appears under EXACTLY ONE key, that
/// key is taken; an inode shared by two devices is ambiguous and matches
/// nothing. A file on a real block device (major != 0 — every idle lock on
/// ext4) never falls back: an idle lock is simply absent from `/proc/locks`,
/// and an inode-only match there would borrow an unrelated file's lock on
/// another filesystem. An [`LockMatch::InodeOnly`] match is still only a
/// candidate — the caller must confirm it by path (see `lock_item`).
pub fn lookup_lock(
    locks: &HashMap<LockKey, LockEntry>,
    key: LockKey,
) -> Option<(LockKey, &LockEntry, LockMatch)> {
    if let Some(e) = locks.get(&key) {
        return Some((key, e, LockMatch::Exact));
    }
    if key.major != 0 {
        return None;
    }
    let mut same_ino = locks.iter().filter(|(k, _)| k.ino == key.ino);
    let (k, e) = same_ino.next()?;
    if same_ino.next().is_some() {
        return None;
    }
    Some((*k, e, LockMatch::InodeOnly))
}

/// Split a Linux `st_dev` into `(major, minor)` — glibc's `gnu_dev_major` /
/// `gnu_dev_minor` bit layout, which is what the kernel's `new_encode_dev`
/// produces for `stat`.
pub fn dev_major_minor(dev: u64) -> (u32, u32) {
    let major = ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0x0000_0fff);
    let minor = ((dev >> 12) & 0xffff_ff00) | (dev & 0x0000_00ff);
    // Both masks keep the value within 32 bits.
    (
        u32::try_from(major).unwrap_or(u32::MAX),
        u32::try_from(minor).unwrap_or(u32::MAX),
    )
}

/// True iff `/proc/<pid>/fdinfo/<fd>` text carries a GRANTED `lock:` line on
/// `key`. The kernel prints, per fd, the locks owned through that fd's open
/// file description, in `/proc/locks` format after a `lock:` prefix.
pub fn fdinfo_holds_lock(fdinfo: &str, key: LockKey) -> bool {
    fdinfo.lines().any(|line| {
        line.strip_prefix("lock:")
            .and_then(parse_proc_locks_line)
            .is_some_and(|(k, _, waiting)| k == key && !waiting)
    })
}

/// A process confirmed to hold a lock: `(pid, ppid, starttime_ticks)`.
pub type OwnerCandidate = (u32, u32, u64);

/// Choose the owner among processes confirmed to hold one lock. Children
/// inherit the fd (`sh -c 'exec 9<f; flock -x 9; cargo …'` shows the lock in
/// both), so the TOPMOST candidate — one whose parent is not a candidate — is
/// the owner; ties break by earliest start, then lowest pid.
pub fn pick_owner(cands: &[OwnerCandidate]) -> Option<u32> {
    let pids: HashSet<u32> = cands.iter().map(|c| c.0).collect();
    cands
        .iter()
        .filter(|(_, ppid, _)| !pids.contains(ppid))
        .min_by_key(|(pid, _, start)| (*start, *pid))
        .map(|c| c.0)
}

// ---------------------------------------------------------------------------
// /proc/<pid> helpers (pure halves).
// ---------------------------------------------------------------------------

/// The fields AFTER `comm` in `/proc/<pid>/stat` (comm may contain spaces and
/// parens, so split at the LAST `)`). Index 0 is field 3 (`state`).
fn stat_tail_fields(stat: &str) -> Option<Vec<&str>> {
    let close = stat.rfind(')')?;
    let tail = stat.get(close + 1..)?;
    Some(tail.split_whitespace().collect())
}

/// `ppid` (field 4) from `/proc/<pid>/stat` text.
pub fn ppid_from_stat(stat: &str) -> Option<u32> {
    stat_tail_fields(stat)?.get(1)?.parse().ok()
}

/// `starttime` (field 22, clock ticks since boot) from `/proc/<pid>/stat` text.
pub fn starttime_ticks_from_stat(stat: &str) -> Option<u64> {
    stat_tail_fields(stat)?.get(19)?.parse().ok()
}

/// A process's age from its start time, the system uptime and the clock-tick
/// rate. Clamped at zero.
pub fn process_age_secs(starttime_ticks: u64, uptime_secs: f64, ticks_per_sec: u64) -> u64 {
    let started = starttime_ticks as f64 / ticks_per_sec.max(1) as f64;
    let age = (uptime_secs - started).max(0.0);
    age as u64
}

/// `/proc/<pid>/cmdline` bytes → the UNTRUNCATED argv. Empty for a kernel
/// thread or an exited process.
pub fn cmdline_argv(raw: &[u8]) -> Vec<String> {
    raw.split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// argv → a printable, truncated command line. `None` when empty.
pub fn argv_display(argv: &[String]) -> Option<String> {
    let joined = argv.join(" ");
    if joined.is_empty() {
        return None;
    }
    Some(joined.chars().take(HOLDER_CMD_MAX_CHARS).collect())
}

fn basename(arg: &str) -> &str {
    arg.rsplit(['/', '\\']).next().unwrap_or(arg)
}

/// True iff `argv` is `… <path>/cargo-target-liveness.sh [--exclude-pid N]… run-locked …`,
/// matched on argv ELEMENTS (never a substring of a joined string).
pub fn is_sweep_argv(argv: &[String]) -> bool {
    let Some(script) = argv.iter().position(|a| basename(a) == SWEEP_SCRIPT) else {
        return false;
    };
    let mut rest = argv.iter().skip(script + 1);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "run-locked" => return true,
            "--exclude-pid" => {
                if rest.next().is_none() {
                    return false;
                }
            }
            _ => return false,
        }
    }
    false
}

/// Classify a holder from its own argv and its ancestors' (`chain[0]` is the
/// holder). `sweep` wins over `build`: a cargo invoked BY the sweeper's
/// `run-locked` is the sweep, not an agent build.
pub fn classify_holder(chain: &[Vec<String>]) -> HolderKind {
    if chain.iter().any(|argv| is_sweep_argv(argv)) {
        return HolderKind::Sweep;
    }
    let argv0 = chain
        .first()
        .and_then(|a| a.first())
        .map_or("", |s| s.as_str());
    let base = basename(argv0);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    const BUILD_TOOLS: &[&str] = &[
        "cargo",
        "rustc",
        "cargo-clippy",
        "clippy-driver",
        "rustdoc",
        "cargo-nextest",
        "cargo-test",
    ];
    if BUILD_TOOLS.contains(&base) {
        HolderKind::Build
    } else {
        HolderKind::Unknown
    }
}

// ---------------------------------------------------------------------------
// Shared-target enumeration.
// ---------------------------------------------------------------------------

/// One lock file to probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockFile {
    pub target_key: String,
    pub path: PathBuf,
}

/// Sorted child DIRECTORIES of `dir` (symlinks not followed — a link out of
/// the tree is not a shared target of this checkout). Missing/unreadable →
/// empty.
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// The shared target dirs of ONE repo checkout: `target`, `target-agent` and
/// each `target-pool/*` under the repo root and its `src-tauri/`.
fn shared_target_dirs(repo_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for base in TARGET_BASES {
        let base_dir = if base.is_empty() {
            repo_root.to_path_buf()
        } else {
            repo_root.join(base)
        };
        for name in SHARED_TARGET_NAMES {
            let t = base_dir.join(name);
            if t.is_dir() {
                out.push(t);
            }
        }
        out.extend(child_dirs(&base_dir.join(TARGET_POOL)));
    }
    out
}

/// `path` relative to `repo_root`, `/`-separated.
fn rel_key(repo_name: &str, repo_root: &Path, dir: &Path) -> Option<String> {
    let rel = dir.strip_prefix(repo_root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(format!("{repo_name}/{}", parts.join("/")))
}

/// Every `.cargo-lock` of every shared target of every canonical repo
/// checkout directly under `workspace_root`, capped at `cap`. Repos are the
/// same set the census anchors on ([`super::census::is_canonical_repo_root`]).
/// The bool is `true` when the cap cut the list short.
pub fn enumerate_shared_lock_files(workspace_root: &Path, cap: usize) -> (Vec<LockFile>, bool) {
    let mut out = Vec::new();
    for repo_root in child_dirs(workspace_root) {
        if !super::census::is_canonical_repo_root(&repo_root) {
            continue;
        }
        let Some(repo_name) = repo_root.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        for target in shared_target_dirs(&repo_root) {
            for level1 in child_dirs(&target) {
                // A profile dir (`debug`) has its own lock; only a dir without
                // one (a `--target <triple>` dir) is descended into.
                let dirs = if level1.join(LOCK_FILE_NAME).is_file() {
                    vec![level1]
                } else {
                    child_dirs(&level1)
                };
                for dir in dirs {
                    let lock = dir.join(LOCK_FILE_NAME);
                    if !lock.is_file() {
                        continue;
                    }
                    let Some(target_key) = rel_key(repo_name, &repo_root, &dir) else {
                        continue;
                    };
                    if out.len() >= cap {
                        return (out, true);
                    }
                    out.push(LockFile {
                        target_key,
                        path: lock,
                    });
                }
            }
        }
    }
    (out, false)
}

// ---------------------------------------------------------------------------
// The probe (Linux).
// ---------------------------------------------------------------------------

/// Probe the workspace the census walks. `None` = UNKNOWN (omit the field).
/// Blocking fs work — call from the blocking pool.
pub fn probe_current() -> Option<Vec<CargoLockItem>> {
    #[cfg(target_os = "linux")]
    {
        let root = super::census::qontinui_root()?;
        probe_workspace(&root)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Epoch of the last cap-truncation warning.
#[cfg(target_os = "linux")]
static LAST_TRUNCATION_LOG_EPOCH: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Epoch of the last fd-scan-truncation warning.
#[cfg(target_os = "linux")]
static LAST_FD_SCAN_LOG_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Run `emit` at most once per hour per `cell`.
#[cfg(target_os = "linux")]
fn hourly(cell: &std::sync::atomic::AtomicU64, emit: impl FnOnce()) {
    use std::sync::atomic::Ordering;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = cell.load(Ordering::Acquire);
    if now.saturating_sub(last) >= 3600
        && cell
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        emit();
    }
}

#[cfg(target_os = "linux")]
fn warn_truncated_throttled(cap: usize) {
    hourly(&LAST_TRUNCATION_LOG_EPOCH, || {
        tracing::warn!(
            "cargo_locks: more than {cap} shared-target lock files — the published list is \
             TRUNCATED to the first {cap} (sorted by repo/target)"
        );
    });
}

/// Probe one workspace root. `None` when `/proc/locks` cannot be read.
#[cfg(target_os = "linux")]
pub fn probe_workspace(workspace_root: &Path) -> Option<Vec<CargoLockItem>> {
    let locks_text = std::fs::read_to_string("/proc/locks").ok()?;
    let locks = parse_proc_locks(&locks_text);
    let (files, truncated) = enumerate_shared_lock_files(workspace_root, MAX_CARGO_LOCK_ITEMS);
    if truncated {
        warn_truncated_throttled(MAX_CARGO_LOCK_ITEMS);
    }
    let mut procs = ProcReader::new();
    let lock_paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    Some(
        files
            .iter()
            .filter_map(|f| lock_item(f, &locks, &mut procs, &lock_paths))
            .collect(),
    )
}

#[cfg(target_os = "linux")]
fn lock_item(
    file: &LockFile,
    locks: &HashMap<LockKey, LockEntry>,
    procs: &mut ProcReader,
    all_lock_paths: &[PathBuf],
) -> Option<CargoLockItem> {
    use std::os::unix::fs::MetadataExt;
    // A lock file that vanished between enumeration and stat is dropped, not
    // reported idle — an idle report would be a fabricated measurement.
    let md = std::fs::metadata(&file.path).ok()?;
    let (major, minor) = dev_major_minor(md.dev());
    let stat_key = LockKey {
        major,
        minor,
        ino: md.ino(),
    };
    let mut matched = lookup_lock(locks, stat_key);
    // An inode-only candidate is accepted ONLY when a process with THIS lock
    // file open (by path) confirms it; otherwise it was someone else's file
    // and this lock is reported idle.
    let mut path_owner = None;
    if let Some((key, e, LockMatch::InodeOnly)) = &matched {
        path_owner = if e.holders.is_empty() {
            None
        } else {
            procs.resolve_owner(*key, &[], &file.path, all_lock_paths)
        };
        if path_owner.is_none() {
            matched = None;
        }
    }
    let (holders, waiters): (&[i32], &[i32]) = match &matched {
        Some((_, e, _)) => (e.holders.as_slice(), e.waiters.as_slice()),
        None => (&[], &[]),
    };

    // `Some(owner)` = held (owner confirmed or not); `None` = idle.
    let held: Option<Option<u32>> = match &matched {
        Some((key, _, how)) if !holders.is_empty() => Some(match how {
            LockMatch::InodeOnly => path_owner,
            LockMatch::Exact => procs.resolve_owner(*key, holders, &file.path, all_lock_paths),
        }),
        // No holder row (none at all, or only `->` waiter rows): the kernel
        // hides a dead taker's row outside the initial pid namespace, so ask
        // the fdinfo of the processes holding THIS file open before calling it
        // idle. Only a CONFIRMED owner makes it held — a process that merely
        // has the file open never does (fdinfo lists only granted locks).
        _ => procs
            .resolve_owner(stat_key, &[], &file.path, all_lock_paths)
            .map(Some),
    };

    let (holder_pid, holder_kind, holder_age_secs, holder_cmd) = match held {
        Some(Some(pid)) => {
            let argv = procs.argv(pid);
            let kind = if argv.is_empty() {
                HolderKind::Unknown
            } else {
                let mut chain = vec![argv.clone()];
                chain.extend(procs.ancestor_argvs(pid));
                classify_holder(&chain)
            };
            (
                Some(pid),
                Some(kind),
                procs.age_secs(pid),
                argv_display(&argv),
            )
        }
        // Held, but no owner could be CONFIRMED: never publish a pid.
        Some(None) => (None, Some(HolderKind::Unknown), None, None),
        None => (None, None, None, None),
    };
    let oldest_wait_secs = waiters
        .iter()
        .filter_map(|p| u32::try_from(*p).ok().filter(|p| *p > 0))
        .filter_map(|p| procs.age_secs(p))
        .max();

    Some(CargoLockItem {
        target_key: file.target_key.clone(),
        holder_pid,
        holder_kind,
        holder_age_secs,
        holder_cmd,
        waiters: u32::try_from(waiters.len()).unwrap_or(u32::MAX),
        oldest_wait_secs,
    })
}

/// Per-tick `/proc` reader: uptime and tick rate read once; `stat` memoized;
/// the open-file index built at most once, on the first unconfirmed holder or
/// row-less lock file (in practice: on most ticks, since an idle lock is one).
#[cfg(target_os = "linux")]
struct ProcReader {
    uptime_secs: Option<f64>,
    ticks_per_sec: u64,
    stats: HashMap<u32, Option<String>>,
    /// Canonical lock path → `(pid, fd)` pairs holding it open.
    open_index: Option<HashMap<PathBuf, Vec<(u32, String)>>>,
}

#[cfg(target_os = "linux")]
impl ProcReader {
    fn new() -> Self {
        let uptime_secs = std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
        // SAFETY: sysconf is a pure libc query with no memory effects.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            uptime_secs,
            ticks_per_sec: u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100),
            stats: HashMap::new(),
            open_index: None,
        }
    }

    fn stat(&mut self, pid: u32) -> Option<&str> {
        self.stats
            .entry(pid)
            .or_insert_with(|| std::fs::read_to_string(format!("/proc/{pid}/stat")).ok())
            .as_deref()
    }

    fn age_secs(&mut self, pid: u32) -> Option<u64> {
        let uptime = self.uptime_secs?;
        let ticks = self.ticks_per_sec;
        let start = starttime_ticks_from_stat(self.stat(pid)?)?;
        Some(process_age_secs(start, uptime, ticks))
    }

    /// NEVER `environ` — only `cmdline`. Empty when unreadable.
    fn argv(&self, pid: u32) -> Vec<String> {
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|raw| cmdline_argv(&raw))
            .unwrap_or_default()
    }

    fn ancestor_argvs(&mut self, pid: u32) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        let mut cur = pid;
        for _ in 0..MAX_ANCESTOR_DEPTH {
            let Some(ppid) = self.stat(cur).and_then(ppid_from_stat) else {
                break;
            };
            if ppid <= 1 || ppid == cur {
                break;
            }
            let argv = self.argv(ppid);
            if !argv.is_empty() {
                out.push(argv);
            }
            cur = ppid;
        }
        out
    }

    /// Does `pid` hold `key` through any of its fds?
    fn pid_holds(pid: u32, key: LockKey) -> bool {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fdinfo")) else {
            return false;
        };
        entries
            .flatten()
            .any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| fdinfo_holds_lock(&t, key)))
    }

    /// The confirmed owner of a granted lock, or `None`.
    ///
    /// 1. The first printed holder (in `/proc/locks` order) that is a real pid
    ///    AND confirmed via its own fdinfo.
    /// 2. Otherwise the processes holding THIS lock file open (by path), each
    ///    confirmed via the fdinfo of that fd; the topmost ([`pick_owner`])
    ///    wins. Waiters need no exclusion: fdinfo lists only granted locks,
    ///    and `/proc/locks` pids are tgids that need not match an fd owner.
    ///
    /// Pass no `holders` to use the path-confirmed arm alone (an inode-only
    /// match, whose key may belong to another file).
    fn resolve_owner(
        &mut self,
        key: LockKey,
        holders: &[i32],
        lock_path: &Path,
        all_lock_paths: &[PathBuf],
    ) -> Option<u32> {
        for pid in holders.iter().filter_map(|p| u32::try_from(*p).ok()) {
            if pid > 0 && Self::pid_holds(pid, key) {
                return Some(pid);
            }
        }
        let canon = std::fs::canonicalize(lock_path).ok()?;
        let openers = self.open_index(all_lock_paths).get(&canon).cloned()?;
        let mut cands: Vec<OwnerCandidate> = Vec::new();
        for (pid, fd) in openers {
            let held = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}"))
                .is_ok_and(|t| fdinfo_holds_lock(&t, key));
            if !held {
                continue;
            }
            let Some(stat) = self.stat(pid) else {
                continue;
            };
            let ppid = ppid_from_stat(stat).unwrap_or(0);
            let start = starttime_ticks_from_stat(stat).unwrap_or(u64::MAX);
            cands.push((pid, ppid, start));
        }
        pick_owner(&cands)
    }

    /// Scan `/proc/*/fd/*` ONCE per tick for fds open on any of the lock
    /// files, bounded by [`MAX_FD_SCAN`]. Processes owned by another uid are
    /// skipped up front (their `fd/` is unreadable to a non-root runner anyway).
    fn open_index(&mut self, lock_paths: &[PathBuf]) -> &HashMap<PathBuf, Vec<(u32, String)>> {
        self.open_index.get_or_insert_with(|| {
            use std::os::unix::fs::MetadataExt;
            // SAFETY: geteuid has no preconditions and cannot fail.
            let my_uid = unsafe { libc::geteuid() };
            let wanted: HashSet<PathBuf> = lock_paths
                .iter()
                .filter_map(|p| std::fs::canonicalize(p).ok())
                .collect();
            let mut index: HashMap<PathBuf, Vec<(u32, String)>> = HashMap::new();
            let mut scanned = 0usize;
            let Ok(procs) = std::fs::read_dir("/proc") else {
                return index;
            };
            'pids: for p in procs.flatten() {
                let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                    continue;
                };
                if !p.metadata().is_ok_and(|m| m.uid() == my_uid) {
                    continue;
                }
                let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
                    continue;
                };
                for fd in fds.flatten() {
                    scanned += 1;
                    if scanned > MAX_FD_SCAN {
                        hourly(&LAST_FD_SCAN_LOG_EPOCH, || {
                            tracing::warn!(
                                "cargo_locks: open-file owner scan stopped at {MAX_FD_SCAN} fds \
                                 — an unconfirmed lock owner may be reported as null"
                            );
                        });
                        break 'pids;
                    }
                    let Ok(target) = std::fs::read_link(fd.path()) else {
                        continue;
                    };
                    if wanted.contains(&target) {
                        let fd_name = fd.file_name().to_string_lossy().into_owned();
                        index.entry(target).or_default().push((pid, fd_name));
                    }
                }
            }
            index
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modeled on the 2026-09-29 sample on merytshost (coord's
    /// `target/debug/.cargo-lock`, one holder and 17 blocked waiters,
    /// interleaved with unrelated FLOCK and POSIX rows).
    const PROC_LOCKS_FIXTURE: &str = "\
1: FLOCK  ADVISORY  READ 3941263 08:01:16019097 0 EOF
2: POSIX  ADVISORY  READ 815964 08:01:16943815 124 124
3: FLOCK  ADVISORY  WRITE 1351229 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1655862 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1655901 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656010 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656122 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656300 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656411 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656522 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656633 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656744 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656855 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1656966 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1657077 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1657188 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1657299 08:01:16669579 0 EOF
3: -> FLOCK  ADVISORY  WRITE 1657300 08:01:16669579 0 EOF
3:  -> FLOCK  ADVISORY  WRITE 1657411 08:01:16669579 0 EOF
   3: -> FLOCK  ADVISORY  WRITE 1657522 08:01:16669579 0 EOF
4: FLOCK  ADVISORY  READ 2391387 08:01:16019097 0 EOF
";

    const COORD_LOCK: LockKey = LockKey {
        major: 8,
        minor: 1,
        ino: 16_669_579,
    };

    fn key(major: u32, minor: u32, ino: u64) -> LockKey {
        LockKey { major, minor, ino }
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parser_names_the_holder_and_every_waiter() {
        let map = parse_proc_locks(PROC_LOCKS_FIXTURE);
        let e = map.get(&COORD_LOCK).expect("coord lock parsed");
        assert_eq!(e.holders, vec![1_351_229]);
        assert_eq!(
            e.waiters.len(),
            17,
            "arrow rows at any indentation are waiters"
        );
        assert_eq!(e.waiters.first(), Some(&1_655_862));
    }

    #[test]
    fn parser_keeps_other_inodes_separate_and_handles_posix_rows() {
        let map = parse_proc_locks(PROC_LOCKS_FIXTURE);
        let shared = map
            .get(&key(8, 1, 16_019_097))
            .expect("shared READ flock parsed");
        assert_eq!(shared.holders, vec![3_941_263, 2_391_387]);
        assert!(shared.waiters.is_empty());
        let posix = map
            .get(&key(8, 1, 16_943_815))
            .expect("POSIX row parsed under its own key");
        assert_eq!(posix.holders, vec![815_964]);
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn parser_skips_malformed_lines() {
        let text = "\
garbage
1: FLOCK ADVISORY WRITE
2: FLOCK  ADVISORY  WRITE notapid 08:01:5 0 EOF
3: FLOCK  ADVISORY  WRITE 42 zz:01:5 0 EOF
5 FLOCK  ADVISORY  WRITE 42 08:01:5 0 EOF
6: FLOCK  ADVISORY  WRITE 42 08:01:5:9 0 EOF
8: FLOCK  ADVISORY  WRITE 99999999999 08:01:5 0 EOF

7: FLOCK  ADVISORY  WRITE 7 00:1f:99 0 EOF
";
        let map = parse_proc_locks(text);
        assert_eq!(map.len(), 1, "only the well-formed row survives: {map:?}");
        let e = map.get(&key(0, 0x1f, 99)).expect("hex minor parsed");
        assert_eq!(e.holders, vec![7]);
    }

    /// W2: pid 0 (outside our pid namespace) and -1 (OFD) rows are KEPT — the
    /// lock is still held / waited on.
    #[test]
    fn parser_keeps_untranslatable_and_ofd_pids() {
        let text = "\
1: OFDLCK ADVISORY  WRITE -1 08:01:5 0 EOF
1: -> FLOCK  ADVISORY  WRITE 0 08:01:5 0 EOF
1: -> FLOCK  ADVISORY  WRITE 44 08:01:5 0 EOF
";
        let map = parse_proc_locks(text);
        let e = map.get(&key(8, 1, 5)).expect("parsed");
        assert_eq!(e.holders, vec![-1]);
        assert_eq!(e.waiters, vec![0, 44]);
    }

    /// W1: inode-only fallback when the device differs, but only when the
    /// inode is unambiguous.
    #[test]
    fn lookup_falls_back_to_a_unique_inode_only() {
        let map = parse_proc_locks(PROC_LOCKS_FIXTURE);
        let (k, _, how) = lookup_lock(&map, COORD_LOCK).expect("exact");
        assert_eq!((k, how), (COORD_LOCK, LockMatch::Exact));
        // A real block device (major != 0) never falls back: an unrelated
        // row with the same inode on another filesystem is NOT this file.
        assert!(
            lookup_lock(&map, key(8, 2, 16_669_579)).is_none(),
            "same inode, different real device must not be attributed"
        );
        assert!(lookup_lock(&map, key(259, 0, 16_669_579)).is_none());
        // btrfs-style: stat reports an anonymous device 00:2f.
        let (k, e, how) = lookup_lock(&map, key(0, 0x2f, 16_669_579)).expect("inode fallback");
        assert_eq!((k, how), (COORD_LOCK, LockMatch::InodeOnly));
        assert_eq!(e.waiters.len(), 17);
        assert!(lookup_lock(&map, key(0, 0x2f, 1)).is_none());

        let ambiguous =
            parse_proc_locks("1: FLOCK  ADVISORY  WRITE 5 08:01:77 0 EOF\n2: FLOCK  ADVISORY  WRITE 6 00:22:77 0 EOF\n");
        assert!(
            lookup_lock(&ambiguous, key(0, 0x2f, 77)).is_none(),
            "an inode on two devices must not match"
        );
    }

    #[test]
    fn fdinfo_lock_lines_confirm_only_the_granted_key() {
        let fdinfo = "pos:\t0\nflags:\t0100000\nmnt_id:\t29\nino:\t16669579\n\
lock:\t1: FLOCK  ADVISORY  WRITE 1351229 08:01:16669579 0 EOF\n";
        assert!(fdinfo_holds_lock(fdinfo, COORD_LOCK));
        assert!(!fdinfo_holds_lock(fdinfo, key(8, 1, 1)));
        assert!(!fdinfo_holds_lock("pos:\t0\nflags:\t0\n", COORD_LOCK));
    }

    #[test]
    fn owner_is_the_topmost_candidate() {
        // sh (10) holds fd 9; its children flock-exited, cargo (12) inherited it.
        assert_eq!(pick_owner(&[(12, 10, 500), (10, 1, 500)]), Some(10));
        // Unrelated candidates: earliest start, then lowest pid.
        assert_eq!(pick_owner(&[(30, 1, 900), (20, 1, 400)]), Some(20));
        assert_eq!(pick_owner(&[(30, 1, 400), (20, 1, 400)]), Some(20));
        assert_eq!(pick_owner(&[]), None);
    }

    #[test]
    fn dev_split_matches_glibc_layout() {
        // makedev(8, 1) = 0x801; makedev(0, 0x36) = 0x36;
        // makedev(259, 70000) exercises both high halves.
        assert_eq!(dev_major_minor(0x801), (8, 1));
        assert_eq!(dev_major_minor(0x36), (0, 0x36));
        let (maj, min) = (259u64, 70_000u64);
        let dev = ((maj & 0xffff_f000) << 32)
            | ((maj & 0x0000_0fff) << 8)
            | ((min & 0xffff_ff00) << 12)
            | (min & 0x0000_00ff);
        assert_eq!(dev_major_minor(dev), (259, 70_000));
    }

    #[test]
    fn stat_fields_survive_a_comm_with_spaces_and_parens() {
        let stat = "1234 (my (odd) proc) S 999 1234 1234 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 5000 1000 100";
        assert_eq!(ppid_from_stat(stat), Some(999));
        assert_eq!(starttime_ticks_from_stat(stat), Some(5000));
        assert_eq!(process_age_secs(5000, 150.0, 100), 100);
        assert_eq!(process_age_secs(90_000, 150.0, 100), 0, "clamped");
    }

    #[test]
    fn cmdline_is_split_untruncated_and_displayed_truncated() {
        let a = cmdline_argv(b"cargo\0test\0-p\0qontinui-coord\0");
        assert_eq!(a, argv(&["cargo", "test", "-p", "qontinui-coord"]));
        assert_eq!(
            argv_display(&a).as_deref(),
            Some("cargo test -p qontinui-coord")
        );
        assert!(cmdline_argv(b"").is_empty());
        assert_eq!(argv_display(&[]), None);
        let long = vec![b'x'; 500];
        let a = cmdline_argv(&long);
        assert_eq!(a.first().map(String::len), Some(500), "argv is untruncated");
        assert_eq!(argv_display(&a).map(|s| s.chars().count()), Some(200));
    }

    #[test]
    fn sweep_is_matched_on_argv_elements() {
        let s = |v: &[&str]| is_sweep_argv(&argv(v));
        assert!(s(&[
            "bash",
            "/x/scripts/cargo-target-liveness.sh",
            "run-locked",
            "/t",
            "5",
            "--",
            "cargo",
            "clean"
        ]));
        assert!(s(&[
            "bash",
            "cargo-target-liveness.sh",
            "--exclude-pid",
            "12",
            "--exclude-pid",
            "13",
            "run-locked",
            "/t"
        ]));
        assert!(!s(&["bash", "/x/cargo-target-liveness.sh", "check", "/t"]));
        assert!(!s(&[
            "bash",
            "/x/cargo-target-liveness.sh",
            "--exclude-pid"
        ]));
        // A substring in some other element is not the sweeper.
        assert!(!s(&["grep", "cargo-target-liveness.sh run-locked", "log"]));
        assert!(!s(&[
            "bash",
            "/x/not-cargo-target-liveness.sh.bak",
            "run-locked"
        ]));
        // Beyond 200 chars of joined argv still matches (untruncated).
        let pad = "p".repeat(300);
        assert!(s(&[
            "bash",
            &pad,
            "/x/cargo-target-liveness.sh",
            "run-locked"
        ]));
    }

    #[test]
    fn holder_classification() {
        assert_eq!(
            classify_holder(&[argv(&["/home/u/.cargo/bin/cargo", "test", "-p", "x"])]),
            HolderKind::Build
        );
        assert_eq!(
            classify_holder(&[argv(&["rustc", "--crate-name", "x"])]),
            HolderKind::Build
        );
        assert_eq!(
            classify_holder(&[
                argv(&["cargo", "clean"]),
                argv(&[
                    "bash",
                    "/x/scripts/cargo-target-liveness.sh",
                    "run-locked",
                    "/t",
                    "5",
                    "--",
                    "cargo",
                    "clean"
                ]),
            ]),
            HolderKind::Sweep,
            "an ancestor's run-locked marks the sweep even when the holder is cargo"
        );
        assert_eq!(
            classify_holder(&[argv(&["python3", "foo.py"])]),
            HolderKind::Unknown
        );
        assert_eq!(classify_holder(&[]), HolderKind::Unknown);
    }

    /// Temp workspace: two canonical repos, one non-canonical dir, one linked
    /// worktree (`.git` file). Returns the root guard.
    fn make_workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let touch = |p: &Path| {
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, b"").expect("write");
        };
        std::fs::create_dir_all(root.join("qontinui-coord/.git")).expect("git dir");
        touch(&root.join("qontinui-coord/target/debug/.cargo-lock"));
        touch(&root.join("qontinui-coord/target/release/.cargo-lock"));
        touch(&root.join("qontinui-coord/target-agent/debug/.cargo-lock"));
        touch(&root.join("qontinui-coord/target-agent/x86_64-unknown-linux-gnu/debug/.cargo-lock"));
        // Not locks: level 2 under a profile dir that has its own lock, deeper
        // than two levels, and a non-shared target name.
        touch(&root.join("qontinui-coord/target/debug/build/.cargo-lock"));
        touch(&root.join("qontinui-coord/target/debug/deps/x/.cargo-lock"));
        touch(&root.join("qontinui-coord/target-other/debug/.cargo-lock"));

        std::fs::create_dir_all(root.join("qontinui-runner/.git")).expect("git dir");
        touch(&root.join("qontinui-runner/target-pool/slot-0/debug/.cargo-lock"));
        touch(&root.join("qontinui-runner/target-pool/slot-1/debug/.cargo-lock"));
        touch(&root.join("qontinui-runner/src-tauri/target/debug/.cargo-lock"));

        // Linked worktree (`.git` FILE) and a non-qontinui dir: not primaries.
        std::fs::create_dir_all(root.join("qontinui-coord-wt-x")).expect("wt");
        std::fs::write(root.join("qontinui-coord-wt-x/.git"), b"gitdir: x").expect("gitfile");
        touch(&root.join("qontinui-coord-wt-x/target/debug/.cargo-lock"));
        std::fs::create_dir_all(root.join("other/.git")).expect("git dir");
        touch(&root.join("other/target/debug/.cargo-lock"));
        tmp
    }

    #[test]
    fn enumeration_covers_shared_targets_of_primary_checkouts_only() {
        let tmp = make_workspace();
        let (files, truncated) = enumerate_shared_lock_files(tmp.path(), MAX_CARGO_LOCK_ITEMS);
        assert!(!truncated);
        let keys: Vec<String> = files.into_iter().map(|f| f.target_key).collect();
        assert_eq!(
            keys,
            vec![
                "qontinui-coord/target/debug",
                "qontinui-coord/target/release",
                "qontinui-coord/target-agent/debug",
                "qontinui-coord/target-agent/x86_64-unknown-linux-gnu/debug",
                "qontinui-runner/target-pool/slot-0/debug",
                "qontinui-runner/target-pool/slot-1/debug",
                "qontinui-runner/src-tauri/target/debug",
            ]
        );
    }

    #[test]
    fn enumeration_is_bounded_and_reports_truncation() {
        let tmp = make_workspace();
        let (files, truncated) = enumerate_shared_lock_files(tmp.path(), 3);
        assert_eq!(files.len(), 3);
        assert!(truncated);
        let (files, truncated) = enumerate_shared_lock_files(tmp.path(), 7);
        assert_eq!(files.len(), 7);
        assert!(!truncated, "exactly at the cap is not a truncation");
    }

    #[test]
    fn serialization_omits_unknown_and_keeps_a_measured_empty() {
        #[derive(Serialize)]
        struct Body {
            #[serde(skip_serializing_if = "Option::is_none")]
            cargo_locks: Option<Vec<CargoLockItem>>,
        }
        let none = serde_json::to_value(Body { cargo_locks: None }).expect("json");
        assert_eq!(none, serde_json::json!({}));
        let empty = serde_json::to_value(Body {
            cargo_locks: Some(vec![]),
        })
        .expect("json");
        assert_eq!(empty, serde_json::json!({ "cargo_locks": [] }));
        let item = CargoLockItem {
            target_key: "qontinui-coord/target/debug".into(),
            holder_pid: None,
            holder_kind: Some(HolderKind::Unknown),
            holder_age_secs: None,
            holder_cmd: None,
            waiters: 0,
            oldest_wait_secs: None,
        };
        assert_eq!(
            serde_json::to_value(&item).expect("json"),
            serde_json::json!({
                "target_key": "qontinui-coord/target/debug",
                "holder_pid": null,
                "holder_kind": "unknown",
                "holder_age_secs": null,
                "holder_cmd": null,
                "waiters": 0,
                "oldest_wait_secs": null
            })
        );
    }

    /// Live: take an exclusive `flock` on a temp `.cargo-lock` from this
    /// process and verify the probe names THIS process as its holder, while
    /// an untouched lock file reports idle.
    #[cfg(target_os = "linux")]
    #[test]
    fn live_probe_reports_an_in_process_flock_holder() {
        use std::os::unix::io::AsRawFd;
        let tmp = make_workspace();
        let held = tmp.path().join("qontinui-coord/target/debug/.cargo-lock");
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&held)
            .expect("open lock");
        // SAFETY: valid fd owned by `f` for the duration of the call.
        let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "flock failed");

        let items = probe_workspace(tmp.path()).expect("/proc/locks readable on Linux");
        let busy = items
            .iter()
            .find(|i| i.target_key == "qontinui-coord/target/debug")
            .expect("held lock reported");
        assert_eq!(busy.holder_pid, Some(std::process::id()));
        assert!(busy.holder_kind.is_some());
        assert!(busy.holder_cmd.is_some(), "own cmdline readable");
        assert!(busy.holder_age_secs.is_some());
        assert_eq!(busy.waiters, 0);

        let idle = items
            .iter()
            .find(|i| i.target_key == "qontinui-coord/target/release")
            .expect("idle lock reported");
        assert_eq!(idle.holder_pid, None);
        assert_eq!(idle.holder_kind, None);
        assert_eq!(idle.waiters, 0);
        assert_eq!(idle.oldest_wait_secs, None);
        drop(f);
    }

    /// Spawn `sh` holding `lock` on fd 9 through a `flock(1)` that EXITS
    /// (the sweeper's shape), with `mode` `-x` or `-s`. Returns the child once
    /// the lock is granted; write a line to its stdin to release it.
    #[cfg(target_os = "linux")]
    fn spawn_inherited_holder(lock: &Path, mode: &str) -> std::process::Child {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec 9<\"$1\"; flock \"$2\" 9 && echo ready; read _x")
            .arg("sh")
            .arg(lock)
            .arg(mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("read ready");
        assert_eq!(line.trim(), "ready");
        child
    }

    #[cfg(target_os = "linux")]
    fn release(mut child: std::process::Child) {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"\n");
        }
        let _ = child.wait();
    }

    /// Kernel-independent: the lock table a WSL2 distro / container shows for a
    /// dead taker is EMPTY for that lock. Drive `lock_item` with an empty table
    /// and require the inherited-fd holder (exclusive AND shared) to be named,
    /// while a file another process merely has OPEN (not locked) stays idle.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_held_lock_with_no_proc_locks_row_is_resolved_through_fdinfo() {
        use std::process::Command;
        if Command::new("flock").arg("--version").output().is_err() {
            eprintln!("flock(1) not installed — skipping the row-less owner test");
            return;
        }
        let tmp = make_workspace();
        let (files, _) = enumerate_shared_lock_files(tmp.path(), MAX_CARGO_LOCK_ITEMS);
        let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        let file_for = |key: &str| {
            files
                .iter()
                .find(|f| f.target_key == key)
                .unwrap_or_else(|| panic!("{key} enumerated"))
        };
        let empty: HashMap<LockKey, LockEntry> = HashMap::new();

        for mode in ["-x", "-s"] {
            let f = file_for("qontinui-coord/target/debug");
            let child = spawn_inherited_holder(&f.path, mode);
            let sh_pid = child.id();
            let item = lock_item(f, &empty, &mut ProcReader::new(), &paths).expect("item");
            release(child);
            assert_eq!(
                item.holder_pid,
                Some(sh_pid),
                "{mode}: a row-less held lock must name the fd holder, not read idle: {item:?}"
            );
            assert!(item.holder_kind.is_some(), "{mode}: held, so a kind is set");
            assert_eq!(item.waiters, 0);
        }

        // No false holder: open, never locked, no row -> idle.
        let f = file_for("qontinui-coord/target/release");
        let opener = std::fs::File::open(&f.path).expect("open without locking");
        let item = lock_item(f, &empty, &mut ProcReader::new(), &paths).expect("item");
        drop(opener);
        assert_eq!(
            item.holder_pid, None,
            "an open-but-unlocked file is idle: {item:?}"
        );
        assert_eq!(item.holder_kind, None);
    }

    /// C1, live: the sweeper's shape. `sh` opens the lock on fd 9 and runs
    /// `flock -x 9` — flock(1) takes the lock and EXITS, so `/proc/locks`
    /// names a dead pid. The probe must name the `sh` that holds the fd, never
    /// the dead taker. Skipped (with a note) where `flock(1)` is absent.
    #[cfg(target_os = "linux")]
    #[test]
    fn live_probe_resolves_an_inherited_fd_owner_when_the_taker_exited() {
        use std::io::{BufRead, BufReader, Write};
        use std::process::{Command, Stdio};
        if Command::new("flock").arg("--version").output().is_err() {
            eprintln!("flock(1) not installed — skipping the inherited-fd owner test");
            return;
        }
        let tmp = make_workspace();
        let held = tmp.path().join("qontinui-coord/target/debug/.cargo-lock");
        // `read` is a builtin, so sh itself (no child) waits on stdin.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exec 9<\"$1\"; flock -x 9 && echo ready; read _x")
            .arg("sh")
            .arg(&held)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sh");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("read ready");
        assert_eq!(line.trim(), "ready");

        let locks = parse_proc_locks(&std::fs::read_to_string("/proc/locks").expect("locks"));
        let items = probe_workspace(tmp.path()).expect("/proc/locks readable on Linux");
        let busy = items
            .iter()
            .find(|i| i.target_key == "qontinui-coord/target/debug")
            .expect("held lock reported");

        // Release the lock and reap the child before asserting.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"\n");
        }
        let _ = child.wait();

        let sh_pid = child.id();
        assert_eq!(
            busy.holder_pid,
            Some(sh_pid),
            "the owner is the sh holding fd 9, not the exited flock(1): {busy:?}"
        );
        assert_eq!(busy.holder_kind, Some(HolderKind::Unknown));
        // And the printed taker really was a different (exited) pid.
        let printed: Vec<i32> = locks
            .values()
            .flat_map(|e| e.holders.iter().copied())
            .collect();
        assert!(
            !printed.contains(&i32::try_from(sh_pid).unwrap_or(-1)),
            "/proc/locks should have printed flock(1)'s pid, not sh's"
        );
    }
}
