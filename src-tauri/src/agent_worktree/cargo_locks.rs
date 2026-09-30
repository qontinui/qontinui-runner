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
//! lock files are `<t>/*/.cargo-lock` and `<t>/*/*/.cargo-lock` (the second
//! level is the `--target <triple>` layout). No deeper recursion, and at most
//! [`MAX_CARGO_LOCK_ITEMS`] items.
//!
//! Idle locks are reported too (holder fields `null`, `waiters: 0`): the item
//! set is the census of shared targets, not only the busy ones.
//!
//! ## How
//!
//! Linux only. `/proc/locks` is read ONCE per tick and parsed by the pure
//! [`parse_proc_locks`] into `(major, minor, inode) → holders + waiters`; a
//! plain row is a granted lock, a `->` row is a process BLOCKED on that same
//! lock. Lock files are matched by `st_dev` / `st_ino`. The holder is named
//! from `/proc/<pid>/cmdline` (never `environ`) and aged from
//! `/proc/<pid>/stat` `starttime` against `/proc/uptime`.
//!
//! ## Wire contract (UNKNOWN is not empty)
//!
//! [`probe_current`] answers `None` on a non-Linux host, when no workspace
//! root resolves, or when `/proc/locks` cannot be read — and the census body
//! then OMITS `cargo_locks`, which coord reads as UNKNOWN. `Some(vec![])` is a
//! measurement: the probe ran and found no shared target lock files.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Ceiling on items per tick. The fleet's workspaces hold a few dozen shared
/// targets; the bound exists so a pathological tree cannot bloat the POST.
pub const MAX_CARGO_LOCK_ITEMS: usize = 256;

/// Truncation of the holder's command line, in chars.
const HOLDER_CMD_MAX_CHARS: usize = 200;

/// How far up the parent chain the `sweep` classification looks.
const MAX_ANCESTOR_DEPTH: usize = 16;

/// Sub-roots of a repo checkout that may host a cargo workspace's targets.
/// `""` is the repo root itself; `src-tauri` is the runner's cargo workspace.
const TARGET_BASES: &[&str] = &["", "src-tauri"];

/// Target dirs under a base that are SHARED (not per-worktree).
const SHARED_TARGET_NAMES: &[&str] = &["target", "target-agent"];

/// Parent dir of the build-pool slots (`target-pool/slot-0`, …).
const TARGET_POOL: &str = "target-pool";

const LOCK_FILE_NAME: &str = ".cargo-lock";

/// What kind of process holds a target lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HolderKind {
    /// A cargo / rustc / clippy / rustdoc process.
    Build,
    /// The claude-config sweeper (`cargo-target-liveness.sh run-locked`), or a
    /// descendant of it.
    Sweep,
    /// Anything else, or a holder whose command line could not be read.
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
    pub holder_pid: Option<u32>,
    pub holder_kind: Option<HolderKind>,
    pub holder_age_secs: Option<u64>,
    pub holder_cmd: Option<String>,
    /// Processes blocked on this lock (`->` rows in `/proc/locks`).
    pub waiters: u32,
    /// Age of the oldest waiting PROCESS — an upper bound on how long it has
    /// waited, since `/proc/locks` does not record when a wait began.
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

/// Every pid granted (`holders`) and blocked on (`waiters`) one lock,
/// in `/proc/locks` order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LockEntry {
    pub holders: Vec<u32>,
    pub waiters: Vec<u32>,
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
/// inode, so unrelated rows are simply never looked up. A row whose pid is not
/// a positive integer (an OFD lock prints `-1`) or that is otherwise malformed
/// is skipped rather than guessed at.
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

/// One row → `(key, pid, is_waiter)`, or `None` for anything malformed.
fn parse_proc_locks_line(line: &str) -> Option<(LockKey, u32, bool)> {
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
    let pid = u32::try_from(pid).ok().filter(|p| *p > 0)?;
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

/// `/proc/<pid>/cmdline` bytes → a printable, truncated command line
/// (NUL-separated argv joined with spaces). `None` when empty (a kernel
/// thread or a process that already exited).
pub fn cmdline_to_string(raw: &[u8]) -> Option<String> {
    let joined = raw
        .split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        return None;
    }
    Some(joined.chars().take(HOLDER_CMD_MAX_CHARS).collect())
}

/// Classify a holder from its own command line and its ancestors'
/// (`chain[0]` is the holder). `sweep` wins over `build`: a cargo invoked BY
/// the sweeper's `run-locked` is the sweep, not an agent build.
pub fn classify_holder(chain: &[String]) -> HolderKind {
    if chain
        .iter()
        .any(|c| c.contains("cargo-target-liveness.sh") && c.contains("run-locked"))
    {
        return HolderKind::Sweep;
    }
    let argv0 = chain
        .first()
        .and_then(|c| c.split_whitespace().next())
        .unwrap_or("");
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
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
pub fn enumerate_shared_lock_files(workspace_root: &Path, cap: usize) -> Vec<LockFile> {
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
                let mut dirs = vec![level1.clone()];
                dirs.extend(child_dirs(&level1));
                for dir in dirs {
                    let lock = dir.join(LOCK_FILE_NAME);
                    if !lock.is_file() {
                        continue;
                    }
                    let Some(target_key) = rel_key(repo_name, &repo_root, &dir) else {
                        continue;
                    };
                    out.push(LockFile {
                        target_key,
                        path: lock,
                    });
                    if out.len() >= cap {
                        return out;
                    }
                }
            }
        }
    }
    out
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

/// Probe one workspace root. `None` when `/proc/locks` cannot be read.
#[cfg(target_os = "linux")]
pub fn probe_workspace(workspace_root: &Path) -> Option<Vec<CargoLockItem>> {
    let locks_text = std::fs::read_to_string("/proc/locks").ok()?;
    let locks = parse_proc_locks(&locks_text);
    let files = enumerate_shared_lock_files(workspace_root, MAX_CARGO_LOCK_ITEMS);
    let mut procs = ProcReader::new();
    Some(
        files
            .into_iter()
            .filter_map(|f| lock_item(&f, &locks, &mut procs))
            .collect(),
    )
}

#[cfg(target_os = "linux")]
fn lock_item(
    file: &LockFile,
    locks: &HashMap<LockKey, LockEntry>,
    procs: &mut ProcReader,
) -> Option<CargoLockItem> {
    use std::os::unix::fs::MetadataExt;
    // A lock file that vanished between enumeration and stat is dropped, not
    // reported idle — an idle report would be a fabricated measurement.
    let md = std::fs::metadata(&file.path).ok()?;
    let (major, minor) = dev_major_minor(md.dev());
    let key = LockKey {
        major,
        minor,
        ino: md.ino(),
    };
    let entry = locks.get(&key);
    let holder_pid = entry.and_then(|e| e.holders.first().copied());
    let waiter_pids: &[u32] = entry.map(|e| e.waiters.as_slice()).unwrap_or(&[]);

    let (holder_kind, holder_age_secs, holder_cmd) = match holder_pid {
        Some(pid) => {
            let cmd = procs.cmdline(pid);
            let kind = match &cmd {
                Some(c) => {
                    let mut chain = vec![c.clone()];
                    chain.extend(procs.ancestor_cmdlines(pid));
                    classify_holder(&chain)
                }
                None => HolderKind::Unknown,
            };
            (Some(kind), procs.age_secs(pid), cmd)
        }
        None => (None, None, None),
    };
    let oldest_wait_secs = waiter_pids.iter().filter_map(|p| procs.age_secs(*p)).max();

    Some(CargoLockItem {
        target_key: file.target_key.clone(),
        holder_pid,
        holder_kind,
        holder_age_secs,
        holder_cmd,
        waiters: u32::try_from(waiter_pids.len()).unwrap_or(u32::MAX),
        oldest_wait_secs,
    })
}

/// Per-tick `/proc` reader: uptime and tick rate read once; `stat` memoized
/// (17 waiters on one lock share ancestors).
#[cfg(target_os = "linux")]
struct ProcReader {
    uptime_secs: Option<f64>,
    ticks_per_sec: u64,
    stats: HashMap<u32, Option<String>>,
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

    /// NEVER `environ` — only `cmdline`.
    fn cmdline(&self, pid: u32) -> Option<String> {
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        cmdline_to_string(&raw)
    }

    fn ancestor_cmdlines(&mut self, pid: u32) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = pid;
        for _ in 0..MAX_ANCESTOR_DEPTH {
            let Some(ppid) = self.stat(cur).and_then(ppid_from_stat) else {
                break;
            };
            if ppid <= 1 || ppid == cur {
                break;
            }
            if let Some(c) = self.cmdline(ppid) {
                out.push(c);
            }
            cur = ppid;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built from the real 2026-09-29 sample on merytshost: coord's
    /// `target/debug/.cargo-lock` (inode 16669579) with one holder and 17
    /// blocked waiters, interleaved with unrelated FLOCK and POSIX rows.
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
            .get(&LockKey {
                major: 8,
                minor: 1,
                ino: 16_019_097,
            })
            .expect("shared READ flock parsed");
        assert_eq!(shared.holders, vec![3_941_263, 2_391_387]);
        assert!(shared.waiters.is_empty());
        let posix = map
            .get(&LockKey {
                major: 8,
                minor: 1,
                ino: 16_943_815,
            })
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
4: OFDLCK ADVISORY  WRITE -1 08:01:5 0 EOF
5 FLOCK  ADVISORY  WRITE 42 08:01:5 0 EOF
6: FLOCK  ADVISORY  WRITE 42 08:01:5:9 0 EOF

7: FLOCK  ADVISORY  WRITE 7 00:1f:99 0 EOF
";
        let map = parse_proc_locks(text);
        assert_eq!(map.len(), 1, "only the well-formed row survives: {map:?}");
        let e = map
            .get(&LockKey {
                major: 0,
                minor: 0x1f,
                ino: 99,
            })
            .expect("hex minor parsed");
        assert_eq!(e.holders, vec![7]);
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
    fn cmdline_is_joined_and_truncated() {
        assert_eq!(
            cmdline_to_string(b"cargo\0test\0-p\0qontinui-coord\0").as_deref(),
            Some("cargo test -p qontinui-coord")
        );
        assert_eq!(cmdline_to_string(b""), None);
        let long = vec![b'x'; 500];
        assert_eq!(
            cmdline_to_string(&long).map(|s| s.chars().count()),
            Some(200)
        );
    }

    #[test]
    fn holder_classification() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            classify_holder(&s(&["/home/u/.cargo/bin/cargo test -p x"])),
            HolderKind::Build
        );
        assert_eq!(
            classify_holder(&s(&["rustc --crate-name x"])),
            HolderKind::Build
        );
        assert_eq!(
            classify_holder(&s(&[
                "cargo clean",
                "bash /x/scripts/cargo-target-liveness.sh run-locked /t -- cargo clean"
            ])),
            HolderKind::Sweep,
            "an ancestor's run-locked marks the sweep even when the holder is cargo"
        );
        assert_eq!(
            classify_holder(&s(&["python3 foo.py"])),
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
        // Not a lock: deeper than two levels, and a non-shared target name.
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
        let keys: Vec<String> = enumerate_shared_lock_files(tmp.path(), MAX_CARGO_LOCK_ITEMS)
            .into_iter()
            .map(|f| f.target_key)
            .collect();
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
    fn enumeration_is_bounded() {
        let tmp = make_workspace();
        assert_eq!(enumerate_shared_lock_files(tmp.path(), 3).len(), 3);
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
            holder_kind: Some(HolderKind::Sweep),
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
                "holder_kind": "sweep",
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
}
