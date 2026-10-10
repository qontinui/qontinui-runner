//! The canonical-generation rung: a runner-owned BARE mirror of
//! `qontinui-claude-config`, fetched off the spawn path, from which the fleet
//! command bodies AND skill directories are served at `origin/main` rather than
//! at whatever this binary happened to embed.
//!
//! ## Why a rung above the embedded bundle
//!
//! The bundled commands (`crate::fleet_commands`) are VENDORED copies of
//! `qontinui-claude-config/.claude/commands/<name>.md`. A running build keeps
//! serving the bytes it was compiled with, so every canonical fix reaches a
//! session only after a re-vendor AND a rebuild of that device's runner — the
//! embedded snapshot is already stale the day it lands. This module serves the
//! canonical repo itself, so a copy that is fresh BY CONSTRUCTION outranks one
//! that is merely labelled stale. The bundle stays the offline floor.
//!
//! Plan `2026-09-03-served-corpus-provenance-at-spawn`, Phases 6 (commands) and
//! 7 (skills).
//!
//! ## Skills carry git's mode bits
//!
//! A skill is a directory, listed with [`Mirror::list_tree`] (`git ls-tree -r
//! -z`), which also yields the MODE git recorded for each file. The embedded
//! floor has to guess executability from a `.sh` extension (`include_dir`
//! carries no permissions); a canonical skill does not guess: a helper is
//! executable exactly when `qontinui-claude-config` committed it `100755`, so an
//! extension-less script or a `.py` helper lands correctly.
//!
//! ## Where it lives, and what it never touches
//!
//! `<config_dir>/com.qontinui.runner[/instance-<name>]/canonical/qontinui-claude-config.git`
//! — the same per-instance base as `agent_commands`' override cache. It is
//! instance STATE, never a managed workspace repo: nothing here reads or writes
//! a checkout under the workspace root, and `crate::fleet`'s
//! `.git/info/exclude` roster does not apply to it.
//!
//! ## Off the spawn path
//!
//! [`refresh`] is ONE bounded `git fetch --depth=1 <url>
//! +refs/heads/main:refs/remotes/origin/main` followed by local plumbing reads.
//! Only the tip is ever read, so the mirror is SHALLOW: its first fetch
//! transfers one tree rather than the repo's whole history, under
//! [`FIRST_FETCH_TIMEOUT`]; every later fetch is an incremental one under
//! [`FETCH_TIMEOUT`], the 20 s class `git_trunk` uses for its periodic reads.
//! Before each fetch the leftovers of a fetch that was killed at its budget
//! (`objects/pack/tmp_*`, stale `*.lock` files) are removed, so an interrupted
//! fetch neither accumulates on disk nor wedges the next one.
//!
//! The load after it is also bounded in SPAWNS, not only in time: ONE `git
//! ls-tree` lists both `.claude/commands` and `.claude/skills` with their modes
//! and object ids, and the blobs are read through `git cat-file --batch`, one
//! spawn per [`BATCH_BYTES`] of content — not one spawn per file.
//!
//! It runs on a background timer ([`start_refresh_loop`], [`REFRESH_INTERVAL`]),
//! never on a spawn. Registry resolution then reads the last published
//! snapshot synchronously through [`latest`] — a lock and an `Arc` clone, no
//! I/O — so the first spawn after boot may see no snapshot and fall to the
//! embedded default, which its provenance key labels `source=builtin`.
//!
//! ## Only a COMPLETE load is published
//!
//! A load separates two kinds of miss. A CONTENT outcome — a path absent at
//! that sha, a symlink or submodule where a file belongs, a body that is not
//! UTF-8, a skill that fails validation — is an answer about the canonical
//! repo, and the load that met it is complete: that unit falls to the next
//! rung. An I/O failure — a timeout, a truncated read, an object the mirror
//! does not hold — says nothing about the repo, so the whole load is refused,
//! the previously published generation stays, and the next tick retries. A
//! degraded corpus is therefore never published, and so can never stick behind
//! the same-sha shortcut.
//!
//! ## Every failure is a fall-through
//!
//! No git, no network, a refused credential, a timeout, a non-zero exit, a
//! truncated read, a malformed sha, and a path absent at that sha all resolve
//! to "this rung did not answer" — the next rung serves, exactly as
//! `crate::provision_guard` documents for its own probe. A refresh failure is
//! logged ONCE per distinct reason (not once per timer tick) and leaves the
//! previously loaded snapshot in place: a stale canonical copy is still newer
//! than the build's own. Nothing here can abort a spawn, because nothing here
//! runs on one.
//!
//! Every object is addressed by the FULL sha or object id, never by a
//! slash-ref: finding `89257638` records MSYS silently mangling
//! `origin/main:.claude/...`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tracing::{info, warn};

use crate::process_helpers::{scrubbed_git, TimedOutput, TimedRun};

/// Budget for an incremental `git fetch` into a mirror that already holds the
/// tracking ref. The same 20 s class `git_trunk::TRUNK_GIT_TIMEOUT` uses for a
/// periodic, off-spawn-path read.
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for the FIRST fetch into an empty mirror. Even shallow, it transfers
/// the whole tip tree (tens of MiB uncompressed), which a slow link cannot do
/// in the incremental budget — and a fetch that is killed every tick never
/// completes at all.
pub(crate) const FIRST_FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Budget for each LOCAL plumbing read against the mirror (`init`,
/// `rev-parse`, `ls-tree`, one `cat-file --batch`). Milliseconds when healthy;
/// the only realistic hang is a lock or a stalled filesystem.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);

/// The most blob content one `git cat-file --batch` is asked for. Half of
/// `process_helpers::MAX_CAPTURED_BYTES`, so a batch's output can never hit the
/// capture cap and read as truncated. The bundled corpus is larger than the
/// cap, which is why the reads are batched rather than issued as one.
const BATCH_BYTES: u64 = (crate::process_helpers::MAX_CAPTURED_BYTES / 2) as u64;

/// How often [`start_refresh_loop`] fetches.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// The next attempt after a tick skipped for contention while this process has
/// published NOTHING yet: the peer holding the lock is about to leave a fresh
/// tracking ref, and waiting a whole [`REFRESH_INTERVAL`] would serve the
/// embedded defaults for fifteen minutes for no reason. A contended tick costs
/// one lock probe, so retrying this often is cheap.
pub(crate) const CONTENDED_RETRY: Duration = Duration::from_secs(30);

/// Environment override for the clone URL — a test or a device whose
/// `qontinui-claude-config` lives somewhere else sets it.
pub(crate) const URL_ENV: &str = "QONTINUI_CANONICAL_CORPUS_URL";

/// The URL used when neither [`URL_ENV`] nor a workspace checkout names one —
/// the same `https://github.com/qontinui/<repo>.git` form `ci_node` uses.
const DEFAULT_URL: &str = "https://github.com/qontinui/qontinui-claude-config.git";

/// The one ref the mirror maintains.
const TRACKING_REF: &str = "refs/remotes/origin/main";

/// The mirror's path under the per-instance runner config dir.
const MIRROR_DIR: &str = "canonical/qontinui-claude-config.git";

/// The file inside the mirror whose OS advisory lock marks its one writer.
/// Git ignores a file it does not know in a git dir, and nothing here ever
/// deletes it: the lock is the kernel's, released when its holder closes the
/// file or dies, so a crashed writer can never leave the mirror locked.
const WRITER_LOCK: &str = "qontinui-writer.lock";

/// How long a refused [`WRITER_LOCK`] is re-tried before this tick reads as
/// contended. A lock is per open file description, and a child some OTHER
/// thread of this process forks shares every descriptor until its `exec`
/// closes the close-on-exec ones — so a lock this process just released can
/// read as held for that instant. A peer's real refresh holds it for a whole
/// fetch, seconds at the least, so this grace never outwaits one.
const WRITER_LOCK_GRACE: Duration = Duration::from_millis(500);

/// The two trees a load reads.
const COMMANDS_DIR: &str = ".claude/commands";
const SKILLS_DIR: &str = ".claude/skills";

/// Which `origin/main` generation a body was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSnapshot {
    /// The FULL 40-hex commit sha `refs/remotes/origin/main` resolved to.
    pub sha: String,
    /// RFC 3339 time of the last SUCCESSFUL fetch that resolved to this sha —
    /// advanced by every such fetch, not only the one that first loaded it.
    pub fetched_at: String,
}

impl CanonicalSnapshot {
    /// The first 12 hex digits — the `canonical_sha=` provenance field.
    pub fn short(&self) -> &str {
        self.sha.get(..12).unwrap_or(&self.sha)
    }
}

/// The bundled commands' bodies as `qontinui-claude-config` holds them at one
/// snapshot. A name absent from `bodies` was absent (or not a regular file)
/// at that sha and falls to the next rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCommands {
    pub snapshot: CanonicalSnapshot,
    /// Command name → body, UTF-8, as committed. NOT yet validated: the
    /// resolver runs each through the same validation an account override
    /// passes.
    pub bodies: BTreeMap<String, String>,
}

/// Git's mode for a regular file.
pub(crate) const MODE_FILE: u32 = 0o100644;
/// Git's mode for an executable file.
pub(crate) const MODE_EXECUTABLE: u32 = 0o100755;

/// Whether git's `mode` is a file this rung will serve. A symlink (`120000`)
/// or a submodule (`160000`) never is.
fn servable_mode(mode: u32) -> bool {
    mode == MODE_FILE || mode == MODE_EXECUTABLE
}

/// One entry of a `git ls-tree -r -l` listing: a blob, or a submodule's
/// gitlink (the only non-blob a recursive listing yields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Full path from the repository root, `/`-separated.
    pub path: String,
    /// The mode git recorded (`0o100644`, `0o100755`, `0o120000`, …).
    pub mode: u32,
    /// The object id.
    pub oid: String,
    /// The blob's size; `None` for a gitlink.
    pub size: Option<u64>,
}

/// One bundled skill as `qontinui-claude-config` holds it at one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSkill {
    /// Relative path → text: the same key space an account unit's `files` map
    /// and the embedded tree use.
    pub files: qontinui_types::agent_text_units::AgentTextUnitFiles,
    /// Relative path → git mode, for every key of [`files`](Self::files).
    /// Only [`MODE_FILE`] and [`MODE_EXECUTABLE`] are ever loaded.
    pub modes: BTreeMap<String, u32>,
}

impl CanonicalSkill {
    /// Whether git recorded `rel_path` as executable.
    pub fn is_executable(&self, rel_path: &str) -> bool {
        self.modes.get(rel_path) == Some(&MODE_EXECUTABLE)
    }
}

/// The bundled skills as `qontinui-claude-config` holds them at one snapshot.
/// A skill absent from `skills` was absent, unreadable or invalid at that sha
/// and falls to the next rung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSkills {
    pub snapshot: CanonicalSnapshot,
    /// Skill name → its files, already through the account layer's
    /// validation (`agent_skills::validate_override`).
    pub skills: BTreeMap<String, CanonicalSkill>,
}

/// Everything one refresh loaded, published as a unit so a reader never sees
/// the commands of one generation beside the skills of another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCorpus {
    pub commands: CanonicalCommands,
    pub skills: CanonicalSkills,
}

impl CanonicalCorpus {
    pub fn snapshot(&self) -> &CanonicalSnapshot {
        &self.commands.snapshot
    }

    /// The same bodies, re-stamped with a later fetch of the same sha.
    fn refetched(&self, snapshot: &CanonicalSnapshot) -> CanonicalCorpus {
        let mut next = self.clone();
        next.commands.snapshot = snapshot.clone();
        next.skills.snapshot = snapshot.clone();
        next
    }
}

/// Proof that this process is the mirror's writer: an exclusive OS advisory
/// lock on [`WRITER_LOCK`], held from before the leftover-clearing through the
/// last read of a load, and released on drop.
///
/// One refresh loop per process ([`start_refresh_loop`]) does not make one
/// writer per mirror: the mirror lives in the per-instance scope, and two
/// processes can resolve the same scope — a bare-launched runner beside the
/// primary. Without this lock each would read the other's in-flight
/// `tmp_pack_*` and ref locks as the leftovers of a killed fetch and delete
/// them.
#[derive(Debug)]
pub(crate) struct WriterLock {
    _file: std::fs::File,
}

/// A handle on one bare mirror: where it lives, what it fetches, and how git
/// is invoked. Production uses [`default_mirror`]; tests point one at a
/// tempdir, a local "remote", and — for the timeout arm — a sleeping program.
#[derive(Debug, Clone)]
pub(crate) struct Mirror {
    git_dir: PathBuf,
    url: String,
    program: OsString,
    fetch_timeout: Duration,
    first_fetch_timeout: Duration,
    local_timeout: Duration,
}

impl Mirror {
    pub(crate) fn new(git_dir: PathBuf, url: String) -> Self {
        Self {
            git_dir,
            url,
            program: OsString::from("git"),
            fetch_timeout: FETCH_TIMEOUT,
            first_fetch_timeout: FIRST_FETCH_TIMEOUT,
            local_timeout: LOCAL_TIMEOUT,
        }
    }

    /// Replace the git program — a nonexistent path or a sleeping script.
    #[cfg(test)]
    pub(crate) fn with_program(mut self, program: impl Into<OsString>) -> Self {
        self.program = program.into();
        self
    }

    /// Set every budget: `fetch` bounds the first fetch and later ones alike.
    #[cfg(test)]
    pub(crate) fn with_timeouts(mut self, fetch: Duration, local: Duration) -> Self {
        self.fetch_timeout = fetch;
        self.first_fetch_timeout = fetch;
        self.local_timeout = local;
        self
    }

    /// A git command addressed at the mirror.
    fn in_mirror(&self) -> std::process::Command {
        let mut cmd = scrubbed_git(&self.program);
        cmd.arg("--git-dir").arg(&self.git_dir);
        cmd
    }

    /// Initialise the bare mirror on first use. Idempotent.
    fn ensure_init(&self) -> Result<(), String> {
        if self.git_dir.join("HEAD").is_file() {
            return Ok(());
        }
        if let Some(parent) = self.git_dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create the mirror's parent dir: {e}"))?;
        }
        let mut cmd = scrubbed_git(&self.program);
        cmd.args(["init", "--bare", "--quiet", "--end-of-options"])
            .arg(&self.git_dir);
        run(cmd, self.local_timeout, "git init --bare").map(|_| ())
    }

    /// Whether the mirror already holds [`TRACKING_REF`] — loose, or packed.
    /// A file read, not a spawn: it only picks the fetch budget.
    fn has_tracking_ref(&self) -> bool {
        if self.git_dir.join(TRACKING_REF).is_file() {
            return true;
        }
        std::fs::read_to_string(self.git_dir.join("packed-refs")).is_ok_and(|packed| {
            packed
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(TRACKING_REF))
        })
    }

    /// Take the mirror's [`WriterLock`] without waiting on a holder — only
    /// the short [`WRITER_LOCK_GRACE`] — creating the mirror dir if need be.
    /// `Ok(None)` when another process holds it — that
    /// process is mid-refresh, so this one must not touch the mirror now.
    /// `Err` when the lock cannot be taken or refused at all (the file cannot
    /// be opened, or the filesystem has no advisory locks).
    pub(crate) fn try_lock_writer(&self) -> Result<Option<WriterLock>, String> {
        std::fs::create_dir_all(&self.git_dir)
            .map_err(|e| format!("could not create the mirror dir: {e}"))?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.git_dir.join(WRITER_LOCK))
            .map_err(|e| format!("could not open the mirror's writer lock: {e}"))?;
        let give_up = std::time::Instant::now() + WRITER_LOCK_GRACE;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(WriterLock { _file: file })),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= give_up {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(format!("could not take the mirror's writer lock: {e}"))
                }
            }
        }
    }

    /// Remove what a fetch killed at its budget leaves behind: the partial
    /// pack `index-pack` was writing (`objects/pack/tmp_*`, which otherwise
    /// stays forever), and the lock files a later fetch would refuse to take.
    ///
    /// Safe only under the [`WriterLock`] the caller must hold: the lock is
    /// what makes this process the mirror's one writer, so nothing — in this
    /// process or another — is mid-write here when a refresh begins. The
    /// fetch it precedes runs with automatic gc and maintenance off, so no
    /// detached `git gc` from an earlier refresh outlives its lock either.
    /// Returns how many entries were removed.
    fn clear_interrupted_fetch(&self, _held: &WriterLock) -> usize {
        let mut removed = 0;
        if let Ok(dir) = std::fs::read_dir(self.git_dir.join("objects").join("pack")) {
            for entry in dir.flatten() {
                if entry.file_name().to_string_lossy().starts_with("tmp_")
                    && std::fs::remove_file(entry.path()).is_ok()
                {
                    removed += 1;
                }
            }
        }
        for lock in [
            "shallow.lock".to_string(),
            "packed-refs.lock".to_string(),
            "config.lock".to_string(),
            format!("{TRACKING_REF}.lock"),
        ] {
            if std::fs::remove_file(self.git_dir.join(lock)).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// [`Self::fetch`] under a freshly taken [`WriterLock`]; an `Err` when
    /// another process holds it.
    pub(crate) fn refresh(&self) -> Result<CanonicalSnapshot, String> {
        match self.try_lock_writer()? {
            Some(held) => self.fetch(&held),
            None => Err("another process is refreshing the mirror".to_string()),
        }
    }

    /// Fetch `main`'s tip from the URL into [`TRACKING_REF`] and resolve it to
    /// a full sha. One network operation: [`FIRST_FETCH_TIMEOUT`] into an
    /// empty mirror, the fetch budget after.
    ///
    /// `gc.auto=0` and `maintenance.auto=false` keep the fetch from starting a
    /// detached `git gc` / `git maintenance`: one would outlive both the
    /// [`WriterLock`] and the child-tree guard, and rewrite packs while the
    /// next writer clears what it takes for leftovers.
    fn fetch(&self, held: &WriterLock) -> Result<CanonicalSnapshot, String> {
        // The network operation itself refuses too, so no caller of the
        // mirror can fetch past the tenant's switch.
        if !crate::egress::permit(crate::egress::Flow::SkillMirror).allowed {
            return Err(SKILL_MIRROR_OFF.to_string());
        }
        self.ensure_init()?;
        let cleared = self.clear_interrupted_fetch(held);
        if cleared > 0 {
            info!(
                "canonical_corpus: removed {cleared} leftover(s) of an interrupted fetch from \
                 the mirror"
            );
        }
        let budget = if self.has_tracking_ref() {
            self.fetch_timeout
        } else {
            self.first_fetch_timeout
        };
        let mut fetch = self.in_mirror();
        fetch
            .args([
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-write-fetch-head",
                "--depth=1",
                "--end-of-options",
            ])
            .arg(&self.url)
            .arg(format!("+refs/heads/main:{TRACKING_REF}"));
        run(fetch, budget, "git fetch")?;

        let mut rev = self.in_mirror();
        rev.args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{TRACKING_REF}^{{commit}}"));
        let out = run(rev, self.local_timeout, "git rev-parse")?;
        let sha = String::from_utf8_lossy(&out).trim().to_string();
        if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{TRACKING_REF} resolved to a non-sha {sha:?}"));
        }
        Ok(CanonicalSnapshot {
            sha: sha.to_ascii_lowercase(),
            fetched_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Every entry under `paths` at `snapshot` — ONE `git ls-tree -r -z -l
    /// --full-tree <sha> -- <paths…>`. A path absent at that sha contributes
    /// nothing; that is a content answer, not an error.
    pub(crate) fn list_tree(
        &self,
        snapshot: &CanonicalSnapshot,
        paths: &[&str],
    ) -> Result<Vec<TreeEntry>, String> {
        let mut cmd = self.in_mirror();
        cmd.args(["ls-tree", "-r", "-z", "-l", "--full-tree"])
            .arg(&snapshot.sha)
            .arg("--")
            .args(paths);
        let out = run(cmd, self.local_timeout, "git ls-tree")?;
        let mut entries = Vec::new();
        for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let record = String::from_utf8_lossy(record);
            // `<mode> SP <type> SP <object> SP+ <size> TAB <path>`
            let (meta, path) = record
                .split_once('\t')
                .ok_or_else(|| format!("git ls-tree: malformed record {record:?}"))?;
            let mut fields = meta.split_whitespace();
            let (Some(mode), Some(_kind), Some(oid), Some(size)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("git ls-tree: malformed record {record:?}"));
            };
            let mode = u32::from_str_radix(mode, 8)
                .map_err(|_| format!("git ls-tree: malformed mode {mode:?}"))?;
            entries.push(TreeEntry {
                path: path.to_string(),
                mode,
                oid: oid.to_string(),
                size: size.parse().ok(),
            });
        }
        Ok(entries)
    }

    /// The contents of every blob in `wanted` (object id → size), read through
    /// `git cat-file --batch` in batches of at most [`BATCH_BYTES`]. Every
    /// requested object must come back whole: a missing object, a malformed
    /// record or a short read is an error, never a partial map.
    pub(crate) fn read_blobs(
        &self,
        wanted: &BTreeMap<String, u64>,
    ) -> Result<HashMap<String, Vec<u8>>, String> {
        let mut out = HashMap::with_capacity(wanted.len());
        let mut batch: Vec<&str> = Vec::new();
        let mut batch_bytes = 0u64;
        for (oid, size) in wanted {
            if !batch.is_empty() && batch_bytes.saturating_add(*size) > BATCH_BYTES {
                self.read_batch(&batch, &mut out)?;
                batch.clear();
                batch_bytes = 0;
            }
            batch.push(oid);
            batch_bytes = batch_bytes.saturating_add(*size);
        }
        if !batch.is_empty() {
            self.read_batch(&batch, &mut out)?;
        }
        Ok(out)
    }

    fn read_batch(&self, oids: &[&str], out: &mut HashMap<String, Vec<u8>>) -> Result<(), String> {
        let mut cmd = self.in_mirror();
        cmd.args(["cat-file", "--batch"]);
        let mut input = Vec::new();
        for oid in oids {
            input.extend_from_slice(oid.as_bytes());
            input.push(b'\n');
        }
        let stdout = run_input(cmd, self.local_timeout, input, "git cat-file --batch")?;
        let objects = parse_batch(&stdout)?;
        for oid in oids {
            if !objects.contains_key(*oid) {
                return Err(format!("git cat-file --batch: {oid} not returned"));
            }
        }
        out.extend(objects);
        Ok(())
    }

    /// Load every bundled command (`.claude/commands/<name>.md`) and skill
    /// (`.claude/skills/<name>/`) at `snapshot`: one `ls-tree`, then the
    /// batched blob reads.
    ///
    /// `Err` means the load could not be COMPLETED (see the module doc) and
    /// nothing from it may be published. A content miss is not an error: that
    /// unit is left out, and the misses are logged as one line per kind.
    pub(crate) fn load(
        &self,
        snapshot: &CanonicalSnapshot,
        command_names: &[&str],
        skill_names: &[&str],
    ) -> Result<CanonicalCorpus, String> {
        let entries = self.list_tree(snapshot, &[COMMANDS_DIR, SKILLS_DIR])?;
        let by_path: HashMap<&str, &TreeEntry> =
            entries.iter().map(|e| (e.path.as_str(), e)).collect();

        let mut missed_commands: Vec<String> = Vec::new();
        let mut command_oids: Vec<(&str, &TreeEntry)> = Vec::new();
        for name in command_names {
            match by_path.get(format!("{COMMANDS_DIR}/{name}.md").as_str()) {
                None => missed_commands.push(format!("{name} (absent)")),
                Some(e) if !servable_mode(e.mode) => {
                    missed_commands.push(format!("{name} (git mode {:o})", e.mode))
                }
                Some(e) => command_oids.push((*name, *e)),
            }
        }

        let mut missed_skills: Vec<String> = Vec::new();
        let mut skill_entries: Vec<(&str, Vec<(String, &TreeEntry)>)> = Vec::new();
        for name in skill_names {
            let prefix = format!("{SKILLS_DIR}/{name}/");
            let files: Vec<(String, &TreeEntry)> = entries
                .iter()
                .filter_map(|e| e.path.strip_prefix(&prefix).map(|rel| (rel.to_string(), e)))
                .collect();
            if files.is_empty() {
                missed_skills.push(format!("{name} (absent)"));
            } else if let Some((rel, e)) = files.iter().find(|(_, e)| !servable_mode(e.mode)) {
                // A half-canonical skill is a `SKILL.md` citing files that are
                // not there: refuse the whole skill.
                missed_skills.push(format!("{name} ({rel} has git mode {:o})", e.mode));
            } else {
                skill_entries.push((*name, files));
            }
        }

        let mut wanted: BTreeMap<String, u64> = BTreeMap::new();
        for e in command_oids.iter().map(|(_, e)| *e).chain(
            skill_entries
                .iter()
                .flat_map(|(_, f)| f.iter().map(|(_, e)| *e)),
        ) {
            wanted.insert(e.oid.clone(), e.size.unwrap_or(0));
        }
        let blobs = self.read_blobs(&wanted)?;
        let blob = |oid: &str| -> Result<&Vec<u8>, String> {
            blobs
                .get(oid)
                .ok_or_else(|| format!("object {oid} was not read"))
        };

        let mut bodies = BTreeMap::new();
        for (name, e) in command_oids {
            match String::from_utf8(blob(&e.oid)?.clone()) {
                Ok(body) => {
                    bodies.insert(name.to_string(), body);
                }
                Err(_) => missed_commands.push(format!("{name} (not UTF-8)")),
            }
        }

        let mut skills = BTreeMap::new();
        for (name, files) in skill_entries {
            let mut texts = qontinui_types::agent_text_units::AgentTextUnitFiles::new();
            let mut modes = BTreeMap::new();
            let mut refused = None;
            for (rel, e) in files {
                match String::from_utf8(blob(&e.oid)?.clone()) {
                    Ok(text) => {
                        modes.insert(rel.clone(), e.mode);
                        texts.insert(rel, text);
                    }
                    Err(_) => {
                        refused = Some(format!("{rel} is not UTF-8"));
                        break;
                    }
                }
            }
            let verdict = match refused {
                Some(why) => Err(why),
                None => validate_skill(snapshot, name, &texts),
            };
            match verdict {
                Ok(()) => {
                    skills.insert(
                        name.to_string(),
                        CanonicalSkill {
                            files: texts,
                            modes,
                        },
                    );
                }
                Err(why) => missed_skills.push(format!("{name} ({why})")),
            }
        }

        for (kind, missed, of) in [
            ("command", &missed_commands, command_names.len()),
            ("skill", &missed_skills, skill_names.len()),
        ] {
            if !missed.is_empty() {
                warn!(
                    "canonical_corpus: {} of {of} bundled {kind}(s) unusable at {} — they fall \
                     to the embedded default: {}",
                    missed.len(),
                    snapshot.short(),
                    missed.join("; ")
                );
            }
        }
        Ok(CanonicalCorpus {
            commands: CanonicalCommands {
                snapshot: snapshot.clone(),
                bodies,
            },
            skills: CanonicalSkills {
                snapshot: snapshot.clone(),
                skills,
            },
        })
    }
}

/// Run a canonical skill's files through the same validation an account
/// skill passes.
fn validate_skill(
    snapshot: &CanonicalSnapshot,
    name: &str,
    files: &qontinui_types::agent_text_units::AgentTextUnitFiles,
) -> Result<(), String> {
    let unit = qontinui_types::agent_text_units::AgentTextUnit {
        id: format!("canonical:{name}"),
        kind: qontinui_types::agent_text_units::AgentTextUnitKind::skill(),
        name: name.to_string(),
        organization_id: None,
        created_by_user_id: None,
        entrypoint: "SKILL.md".to_string(),
        files: files.clone(),
        checksum: None,
        is_shared: false,
        is_invocable: true,
        current_version: 1,
        source: "canonical".to_string(),
        source_path: Some(format!("{SKILLS_DIR}/{name}")),
        source_commit: Some(snapshot.sha.clone()),
        created_at: snapshot.fetched_at.clone(),
        updated_at: snapshot.fetched_at.clone(),
    };
    crate::agent_skills::validate_override(&unit, crate::agent_skills::AgentSkillSource::Builtin)
        .map(|_| ())
}

/// Parse `git cat-file --batch` output: per object, `<oid> <type> <size>\n`,
/// `<size>` bytes, `\n` — or `<name> missing\n`, which is an error here,
/// because every name asked for came from an `ls-tree` of the same mirror.
fn parse_batch(mut rest: &[u8]) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut objects = HashMap::new();
    while !rest.is_empty() {
        let nl = rest
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("git cat-file --batch: unterminated header")?;
        let header = String::from_utf8_lossy(rest.get(..nl).unwrap_or_default()).into_owned();
        let mut fields = header.split(' ');
        let (Some(oid), Some(kind), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("git cat-file --batch: {header:?}"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("git cat-file --batch: malformed size in {header:?}"))?;
        let body_start = nl + 1;
        let body_end = body_start
            .checked_add(size)
            .ok_or("git cat-file --batch: size overflow")?;
        let body = rest
            .get(body_start..body_end)
            .ok_or_else(|| format!("git cat-file --batch: {oid} is short"))?;
        if rest.get(body_end) != Some(&b'\n') {
            return Err(format!("git cat-file --batch: {oid} is not terminated"));
        }
        if kind != "blob" {
            return Err(format!("git cat-file --batch: {oid} is a {kind}"));
        }
        objects.insert(oid.to_string(), body.to_vec());
        rest = rest.get(body_end + 1..).unwrap_or_default();
    }
    Ok(objects)
}

/// Run `cmd` under `timeout` through the crate's bounded runner, returning its
/// stdout only when it exited 0 AND its output was read in full. `what` is a
/// fixed, caller-authored label — never the argv, which may carry a URL with
/// a credential in it.
fn run(cmd: std::process::Command, timeout: Duration, what: &str) -> Result<Vec<u8>, String> {
    finish(
        crate::process_helpers::run_with_timeout_detailed(cmd, timeout),
        timeout,
        what,
    )
}

/// [`run`] for a command that reads `input` on stdin.
fn run_input(
    cmd: std::process::Command,
    timeout: Duration,
    input: Vec<u8>,
    what: &str,
) -> Result<Vec<u8>, String> {
    finish(
        crate::process_helpers::run_with_timeout_input(cmd, timeout, input),
        timeout,
        what,
    )
}

fn finish(
    result: std::io::Result<TimedRun>,
    timeout: Duration,
    what: &str,
) -> Result<Vec<u8>, String> {
    match result {
        Err(e) => Err(format!("{what}: git unavailable ({e})")),
        Ok(TimedRun {
            outcome: TimedOutput::TimedOut { .. },
            ..
        }) => Err(format!("{what}: timed out after {timeout:?}")),
        Ok(TimedRun {
            truncation: Some(reason),
            ..
        }) => Err(format!("{what}: output truncated ({reason:?})")),
        Ok(TimedRun {
            outcome: TimedOutput::Completed(out),
            truncation: None,
        }) => {
            if out.status.success() {
                Ok(out.stdout)
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let first = stderr.lines().next().unwrap_or("").trim();
                Err(format!("{what}: exited {} ({first})", out.status))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The process-wide snapshot
// ---------------------------------------------------------------------------

/// A published corpus plus the last refresh failure reason. The process has
/// one ([`LATEST`]); a test builds its own, so it never races the global.
pub(crate) struct Published {
    corpus: RwLock<Option<Arc<CanonicalCorpus>>>,
    /// So a failure repeating on every timer tick is logged once rather than
    /// every fifteen minutes forever.
    last_failure: Mutex<Option<String>>,
    /// So a tick skipped because another process holds the mirror is logged
    /// once per run of such ticks, not on every one.
    contention_logged: std::sync::atomic::AtomicBool,
    /// Whether the LAST tick was skipped for contention — what
    /// [`Self::next_delay`] reads.
    last_tick_contended: std::sync::atomic::AtomicBool,
}

impl Published {
    pub(crate) const fn new() -> Self {
        Self {
            corpus: RwLock::new(None),
            last_failure: Mutex::new(None),
            contention_logged: std::sync::atomic::AtomicBool::new(false),
            last_tick_contended: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn get(&self) -> Option<Arc<CanonicalCorpus>> {
        self.corpus
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set(&self, corpus: CanonicalCorpus) {
        *self
            .corpus
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(corpus));
    }

    fn snapshot(&self) -> Option<CanonicalSnapshot> {
        self.get().map(|c| c.snapshot().clone())
    }

    fn recovered(&self) {
        self.contention_logged
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let mut last = self
            .last_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(prev) = last.take() {
            info!("canonical_corpus: refresh recovered (previously: {prev})");
        }
    }

    /// A tick skipped because another process holds the mirror's
    /// [`WriterLock`]. Not a failure: that process is refreshing the same
    /// mirror, and the next tick here loads whatever it fetched.
    fn contended(&self) {
        if !self
            .contention_logged
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            info!(
                "canonical_corpus: another process is refreshing this instance's mirror; \
                 skipping this refresh (logged once until a refresh here succeeds)"
            );
        }
    }

    /// How long the refresh loop waits before its next tick:
    /// [`CONTENDED_RETRY`] when the last tick was skipped for contention AND
    /// nothing has been published yet, else [`REFRESH_INTERVAL`]. A process
    /// that already serves a generation loses nothing by waiting the full
    /// interval; one on the embedded defaults would.
    pub(crate) fn next_delay(&self) -> Duration {
        if self
            .last_tick_contended
            .load(std::sync::atomic::Ordering::Relaxed)
            && self.get().is_none()
        {
            CONTENDED_RETRY
        } else {
            REFRESH_INTERVAL
        }
    }

    /// The tenant's `egress_skill_mirror` switch is off: serve the EMBEDDED
    /// floor. A generation fetched before the flip is unpublished too, so what
    /// a session is served while the switch is off never depends on whether
    /// this process happened to fetch earlier. Logged once per run of
    /// switched-off ticks (it shares the failure de-duplication slot, so the
    /// first successful fetch afterwards logs the recovery).
    fn egress_off(&self) {
        *self
            .corpus
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let mut last = self
            .last_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.as_deref() != Some(SKILL_MIRROR_OFF) {
            info!(
                "canonical_corpus: {SKILL_MIRROR_OFF} — not fetching; sessions are served the \
                 embedded bundle"
            );
            *last = Some(SKILL_MIRROR_OFF.to_string());
        }
    }

    fn failed(&self, why: String) {
        let mut last = self
            .last_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if last.as_deref() != Some(why.as_str()) {
            warn!(
                "canonical_corpus: refresh failed ({why}) — sessions keep the {} and fall back \
                 to the embedded defaults for anything it lacks",
                match self.snapshot() {
                    Some(s) => format!("previously loaded snapshot {}", s.short()),
                    None => "embedded defaults".to_string(),
                }
            );
            *last = Some(why);
        }
    }
}

/// The process's published corpus. `None` until the first complete load.
static LATEST: Published = Published::new();

/// Why nothing is fetched while the tenant's switch is off — also the reason
/// the served-corpus header names.
pub(crate) const SKILL_MIRROR_OFF: &str = "egress_skill_mirror is off for this project";

/// The last loaded corpus, for synchronous registry resolution. No I/O.
pub(crate) fn latest() -> Option<Arc<CanonicalCorpus>> {
    LATEST.get()
}

/// The production mirror: under the per-instance runner config dir, fetching
/// the URL [`resolve_url`] names. `None` when the platform has no config dir.
fn default_mirror() -> Option<Mirror> {
    let base = dirs::config_dir()?.join("com.qontinui.runner");
    let git_dir = crate::instance::scope_path(&base).join(MIRROR_DIR);
    Some(Mirror::new(git_dir, resolve_url()))
}

/// The clone URL: [`URL_ENV`] when set, else the workspace's own
/// `qontinui-claude-config` checkout's `remote.origin.url` (so a device whose
/// fleet checkout uses SSH or a fork fetches the same way it already does),
/// else [`DEFAULT_URL`].
fn resolve_url() -> String {
    if let Some(url) = std::env::var(URL_ENV)
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
    {
        return url;
    }
    if let Some(checkout) = crate::workspace_paths::workspace_root_readonly()
        .map(|root| root.join("qontinui-claude-config"))
        .filter(|dir| dir.join(".git").exists())
    {
        let mut cmd = scrubbed_git(OsStr::new("git"));
        cmd.arg("-C")
            .arg(&checkout)
            .args(["config", "--get", "remote.origin.url"]);
        if let Ok(out) = run(cmd, LOCAL_TIMEOUT, "git config remote.origin.url") {
            let url = String::from_utf8_lossy(&out).trim().to_string();
            if !url.is_empty() {
                return url;
            }
        }
    }
    DEFAULT_URL.to_string()
}

/// Every bundled command name — the set the canonical rung may replace.
fn bundled_command_names() -> Vec<&'static str> {
    crate::fleet_commands::FLEET_COMMANDS
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

/// Every bundled skill name — the set the canonical rung may replace.
fn bundled_skill_names() -> Vec<String> {
    crate::fleet_skills::embedded_skills()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

/// Refresh `mirror` into `published`, loading `command_names` and
/// `skill_names`. Returns the snapshot `published` now serves (`None` when
/// nothing has ever loaded).
///
/// - A fetch that resolves to the published sha keeps the published bodies
///   and advances their `fetched_at`: it is the last SUCCESSFUL fetch.
/// - A new sha is published only when its load COMPLETED. An incomplete load
///   keeps the previous generation, and because it was never published, the
///   next tick sees a sha that still differs and retries it.
/// - A failed fetch keeps the previous generation: a stale canonical copy is
///   still the newest one this device can read.
/// - A mirror whose [`WriterLock`] another process holds is not touched at
///   all this tick — no clear, no fetch, no load — and the previous
///   generation keeps serving; that is contention, not a failure. Its cost is
///   staleness until the next tick: a full [`REFRESH_INTERVAL`] when a
///   generation is already published, but only [`CONTENDED_RETRY`] when
///   nothing is (see [`Published::next_delay`]), so a process that loses the
///   race at boot serves the embedded defaults for seconds, not minutes.
pub(crate) fn refresh_into(
    mirror: &Mirror,
    published: &Published,
    command_names: &[&str],
    skill_names: &[&str],
) -> Option<CanonicalSnapshot> {
    // The tenant's `egress_skill_mirror` switch (plan
    // 2026-10-10-spec-front-end-phase-9-generic-boundary, Phase 7), checked
    // every tick before the mirror is touched: off means the embedded floor,
    // and no fetch.
    if !crate::egress::permit_or_count(crate::egress::Flow::SkillMirror) {
        published.egress_off();
        return None;
    }
    let lock = mirror.try_lock_writer();
    published.last_tick_contended.store(
        matches!(lock, Ok(None)),
        std::sync::atomic::Ordering::Relaxed,
    );
    // Held to the end of this function: the clear, the fetch AND the load.
    let held = match lock {
        Ok(Some(held)) => held,
        Ok(None) => {
            published.contended();
            return published.snapshot();
        }
        Err(why) => {
            published.failed(why);
            return published.snapshot();
        }
    };
    let snapshot = match mirror.fetch(&held) {
        Ok(snapshot) => snapshot,
        Err(why) => {
            published.failed(why);
            return published.snapshot();
        }
    };
    if let Some(current) = published.get() {
        if current.snapshot().sha == snapshot.sha {
            published.set(current.refetched(&snapshot));
            published.recovered();
            return Some(snapshot);
        }
    }
    match mirror.load(&snapshot, command_names, skill_names) {
        Ok(corpus) => {
            info!(
                "canonical_corpus: serving qontinui-claude-config@{} ({} of {} bundled \
                 command(s), {} of {} bundled skill(s) present)",
                snapshot.short(),
                corpus.commands.bodies.len(),
                command_names.len(),
                corpus.skills.skills.len(),
                skill_names.len(),
            );
            published.set(corpus);
            published.recovered();
            Some(snapshot)
        }
        Err(why) => {
            published.failed(format!(
                "the load at {} did not complete, so it was not published: {why}",
                snapshot.short()
            ));
            published.snapshot()
        }
    }
}

/// One refresh of the production mirror into the process's [`LATEST`].
/// Blocking — call it off the async runtime's worker threads.
pub(crate) fn refresh() -> Option<CanonicalSnapshot> {
    let mirror = default_mirror()?;
    let skill_names = bundled_skill_names();
    let skill_refs: Vec<&str> = skill_names.iter().map(String::as_str).collect();
    refresh_into(&mirror, &LATEST, &bundled_command_names(), &skill_refs)
}

/// Whether [`start_refresh_loop`] has already started this process's loop.
static LOOP_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start the background refresher: a short boot grace, then a refresh every
/// [`Published::next_delay`] — [`REFRESH_INTERVAL`], or [`CONTENDED_RETRY`]
/// while a peer holds the mirror and nothing is published — each on the
/// blocking pool. Hung beside the embedded
/// defaults publisher in `mcp_api::create_router`, never on a spawn path. Once
/// per process: a second call (a second router) is a no-op. That keeps one
/// loop per PROCESS; one writer per MIRROR — what makes
/// [`Mirror::clear_interrupted_fetch`] safe — is the [`WriterLock`] each
/// refresh takes, since two processes can share a mirror.
pub(crate) fn start_refresh_loop() {
    if LOOP_STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            if let Err(e) = tokio::task::spawn_blocking(refresh).await {
                warn!("canonical_corpus: refresh task panicked ({e})");
            }
            tokio::time::sleep(LATEST.next_delay()).await;
        }
    });
}

/// Test fixtures shared with the resolvers' tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::*;

    /// Run git in `dir` with no dependence on the user's global hooks,
    /// signing config or locale, and with the caller's repository
    /// environment removed.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The local "remote" under `root`.
    pub(crate) fn remote_dir(root: &Path) -> PathBuf {
        root.join("remote")
    }

    /// A local "remote": a non-bare repo on branch `main` holding `files`
    /// (relative path → contents), one commit. Returns its path as the URL.
    pub(crate) fn remote_with(root: &Path, files: &[(&str, &str)]) -> String {
        let remote = remote_dir(root);
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--quiet", "--initial-branch=main"]);
        commit_files(root, files);
        remote.to_string_lossy().into_owned()
    }

    /// Write `files` into the remote and commit them — a new `main` tip.
    pub(crate) fn commit_files(root: &Path, files: &[(&str, &str)]) {
        let remote = remote_dir(root);
        for (path, text) in files {
            let dst = remote.join(path);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::write(&dst, text).unwrap();
        }
        git(&remote, &["add", "--all"]);
        git(
            &remote,
            &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
        );
    }

    /// Mark `path` executable in the remote's index and commit it, so git
    /// records mode `100755` regardless of the filesystem's own bits.
    pub(crate) fn chmod_x(root: &Path, path: &str) {
        let remote = remote_dir(root);
        git(&remote, &["update-index", "--chmod=+x", "--", path]);
        git(&remote, &["commit", "--quiet", "-m", "chmod"]);
    }

    /// A mirror under `root` fetching `url`.
    pub(crate) fn mirror(root: &Path, url: &str) -> Mirror {
        Mirror::new(root.join("mirror.git"), url.to_string())
    }

    /// Write an executable shell script at `path` and return only once it can
    /// be exec'd. A concurrently forking test thread can briefly inherit the
    /// script's write descriptor, and an exec in that window fails with
    /// ETXTBSY — which would read as "git unavailable" rather than as the
    /// behaviour under test.
    #[cfg(unix)]
    pub(crate) fn executable_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match Command::new(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut c) => {
                    let _ = c.kill();
                    let _ = c.wait();
                    return;
                }
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("cannot exec the test script: {e}"),
            }
        }
        panic!("the test script stayed ETXTBSY for 2 s");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::test_support::{commit_files, git, mirror, remote_dir, remote_with};
    use super::*;

    /// Load `commands` (and no skills) at `snap`.
    fn commands_at(m: &Mirror, snap: &CanonicalSnapshot, commands: &[&str]) -> CanonicalCommands {
        m.load(snap, commands, &[]).expect("load").commands
    }

    #[test]
    fn refresh_resolves_the_full_sha_and_reads_by_it() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().expect("refresh");
        assert_eq!(snap.sha.len(), 40);
        assert_eq!(snap.short().len(), 12);
        assert_eq!(
            commands_at(&m, &snap, &["vet-plan"]).bodies["vet-plan"],
            "# canon\n"
        );
        // A second refresh over the initialised mirror is idempotent.
        assert_eq!(m.refresh().unwrap().sha, snap.sha);
        // The mirror is bare and runner-owned: no working tree was created.
        assert!(tmp.path().join("mirror.git").join("HEAD").is_file());
        assert!(!tmp.path().join("mirror.git").join(".claude").exists());
    }

    /// Only the tip is fetched: the mirror is shallow, holds no ancestor, and a
    /// later shallow fetch follows `main` and reads the new tip by full sha.
    #[test]
    fn the_mirror_is_shallow_and_follows_the_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# one\n")]);
        commit_files(tmp.path(), &[(".claude/commands/vet-plan.md", "# two\n")]);
        let parent = git(&remote_dir(tmp.path()), &["rev-parse", "HEAD~1"]);
        let m = mirror(tmp.path(), &url);
        assert!(
            !m.has_tracking_ref(),
            "an empty mirror takes the first-fetch budget"
        );
        let first = m.refresh().expect("first fetch");
        assert!(m.has_tracking_ref());
        let git_dir = tmp.path().join("mirror.git");
        assert!(
            git_dir.join("shallow").is_file(),
            "the mirror must be shallow"
        );
        let count = git(&git_dir, &["rev-list", "--count", &first.sha]);
        assert_eq!(count.trim(), "1", "only the tip commit is held");
        let has_parent = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .args(["cat-file", "-e", parent.trim()])
            .status()
            .unwrap();
        assert!(!has_parent.success(), "no history was fetched");
        assert_eq!(
            commands_at(&m, &first, &["vet-plan"]).bodies["vet-plan"],
            "# two\n"
        );

        commit_files(tmp.path(), &[(".claude/commands/vet-plan.md", "# three\n")]);
        let second = m.refresh().expect("incremental shallow fetch");
        assert_ne!(second.sha, first.sha);
        assert_eq!(
            commands_at(&m, &second, &["vet-plan"]).bodies["vet-plan"],
            "# three\n"
        );
    }

    /// A fetch killed at its budget leaves a partial pack and locks; the next
    /// refresh removes them and succeeds.
    #[test]
    fn leftovers_of_an_interrupted_fetch_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        m.refresh().expect("refresh");
        let git_dir = tmp.path().join("mirror.git");
        let leftovers = [
            git_dir.join("objects/pack/tmp_pack_Ab12Cd"),
            git_dir.join("objects/pack/tmp_idx_Ab12Cd"),
            git_dir.join("shallow.lock"),
            git_dir.join(format!("{TRACKING_REF}.lock")),
        ];
        for path in &leftovers {
            std::fs::write(path, b"partial").unwrap();
        }
        commit_files(tmp.path(), &[(".claude/commands/vet-plan.md", "# next\n")]);
        let snap = m.refresh().expect("a refresh past the leftovers");
        for path in &leftovers {
            assert!(!path.exists(), "{} must be removed", path.display());
        }
        assert_eq!(
            commands_at(&m, &snap, &["vet-plan"]).bodies["vet-plan"],
            "# next\n"
        );
    }

    /// A URL is a positional, never an option: `--end-of-options` stops a
    /// `-`-leading URL from being read as `--upload-pack=<command>`.
    #[test]
    fn a_dash_leading_url_is_never_an_option() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("executed");
        let m = mirror(
            tmp.path(),
            &format!("--upload-pack=touch {}", marker.display()),
        );
        assert!(m.refresh().is_err());
        assert!(!marker.exists(), "the URL must not have run as a command");
    }

    /// Every git this module runs speaks the C locale and cannot be
    /// redirected at another repository.
    #[test]
    fn the_git_builder_scrubs_the_repository_env_and_pins_the_locale() {
        let cmd = scrubbed_git(OsStr::new("git"));
        let envs: HashMap<_, _> = cmd.get_envs().collect();
        assert_eq!(envs.get(OsStr::new("LC_ALL")), Some(&Some(OsStr::new("C"))));
        assert_eq!(
            envs.get(OsStr::new("LANGUAGE")),
            Some(&Some(OsStr::new("C")))
        );
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
        ] {
            assert_eq!(envs.get(OsStr::new(var)), Some(&None), "{var} is removed");
        }
    }

    #[test]
    fn a_path_absent_at_the_sha_is_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().unwrap();
        let loaded = commands_at(&m, &snap, &["vet-plan", "absent"]);
        assert_eq!(loaded.bodies.len(), 1);
        assert_eq!(loaded.bodies["vet-plan"], "# canon\n");
        assert_eq!(loaded.snapshot, snap);
    }

    /// A command committed as a symlink or a submodule is refused — the same
    /// mode filter skills pass — and so is a skill holding either.
    #[cfg(unix)]
    #[test]
    fn a_symlink_or_submodule_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(
            tmp.path(),
            &[
                (".claude/commands/target.md", "# target\n"),
                (".claude/commands/plain.md", "# plain\n"),
                (
                    ".claude/skills/demo/SKILL.md",
                    "---\nname: demo\n---\n# demo\n",
                ),
                (
                    ".claude/skills/linked/SKILL.md",
                    "---\nname: linked\n---\n# linked\n",
                ),
            ],
        );
        let remote = remote_dir(tmp.path());
        std::os::unix::fs::symlink("target.md", remote.join(".claude/commands/linky.md")).unwrap();
        let head = git(&remote, &["rev-parse", "HEAD"]);
        git(
            &remote,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},.claude/commands/sub.md", head.trim()),
            ],
        );
        git(
            &remote,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},.claude/skills/linked/vendored", head.trim()),
            ],
        );
        git(&remote, &["add", "--", ".claude/commands/linky.md"]);
        git(&remote, &["commit", "--quiet", "-m", "links"]);

        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().unwrap();
        let corpus = m
            .load(&snap, &["plain", "linky", "sub"], &["demo", "linked"])
            .expect("a refused unit is a content answer, not a failed load");
        assert_eq!(
            corpus.commands.bodies.keys().collect::<Vec<_>>(),
            vec!["plain"]
        );
        assert_eq!(
            corpus.skills.skills.keys().collect::<Vec<_>>(),
            vec!["demo"]
        );
    }

    /// The bundled corpus is larger than one capture can hold; the batched
    /// read loads content past that cap in full.
    #[test]
    fn a_corpus_larger_than_one_capture_loads_in_full() {
        let tmp = tempfile::tempdir().unwrap();
        let size = crate::process_helpers::MAX_CAPTURED_BYTES * 3 / 8;
        let bodies: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| format!("# {n}\n{}\n", n.repeat(size)))
            .collect();
        let paths: Vec<String> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| format!(".claude/commands/{n}.md"))
            .collect();
        let files: Vec<(&str, &str)> = paths
            .iter()
            .map(String::as_str)
            .zip(bodies.iter().map(String::as_str))
            .collect();
        let m = mirror(tmp.path(), &remote_with(tmp.path(), &files));
        let snap = m.refresh().unwrap();
        let loaded = commands_at(&m, &snap, &["a", "b", "c", "d"]);
        assert_eq!(loaded.bodies.len(), 4);
        assert_eq!(loaded.bodies["d"], bodies[3]);
    }

    #[test]
    fn a_malformed_batch_is_an_error_not_a_partial_map() {
        assert!(parse_batch(b"abc blob 3\nxyz\n").is_ok());
        assert!(parse_batch(b"abc missing\n").is_err());
        assert!(parse_batch(b"abc blob 9\nxyz\n").is_err(), "short read");
        assert!(parse_batch(b"abc blob 3\nxyzq").is_err(), "unterminated");
        assert!(parse_batch(b"abc tree 3\nxyz\n").is_err());
    }

    #[test]
    fn an_unreachable_url_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mirror(
            tmp.path(),
            &tmp.path().join("no-such-remote").to_string_lossy(),
        );
        let err = m.refresh().expect_err("nothing to fetch");
        assert!(err.contains("git fetch"), "{err}");
    }

    #[test]
    fn a_missing_git_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mirror(tmp.path(), "unused").with_program(tmp.path().join("no-such-git"));
        let err = m.refresh().expect_err("no git");
        assert!(err.contains("git unavailable"), "{err}");
    }

    // -- publication ---------------------------------------------------------

    /// A same-sha fetch is still the last successful fetch: `fetched_at`
    /// advances and the loaded bodies are kept.
    #[test]
    fn a_same_sha_fetch_advances_fetched_at_and_keeps_the_bodies() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let published = Published::new();
        let first = refresh_into(&m, &published, &["vet-plan"], &[]).expect("published");
        let before = published.get().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let second = refresh_into(&m, &published, &["vet-plan"], &[]).expect("published");
        assert_eq!(second.sha, first.sha);
        assert!(
            second.fetched_at > first.fetched_at,
            "{} must advance past {}",
            second.fetched_at,
            first.fetched_at
        );
        let after = published.get().unwrap();
        assert_eq!(after.snapshot(), &second);
        assert_eq!(after.skills.snapshot, second);
        assert_eq!(after.commands.bodies, before.commands.bodies);
    }

    /// A load that cannot complete is never published: the previous
    /// generation keeps serving, and the next tick retries the new sha.
    #[cfg(unix)]
    #[test]
    fn a_load_that_cannot_complete_keeps_the_previous_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# one\n")]);
        let flag = tmp.path().join("fail-reads");
        let wrapper = tmp.path().join("flaky-git");
        test_support::executable_script(
            &wrapper,
            &format!(
                "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = cat-file ] && [ -e '{}' ]; then \
                 exit 1; fi\ndone\nexec git \"$@\"\n",
                flag.display()
            ),
        );
        let m = mirror(tmp.path(), &url).with_program(&wrapper);
        let published = Published::new();
        let one = refresh_into(&m, &published, &["vet-plan"], &[]).expect("first load");

        commit_files(tmp.path(), &[(".claude/commands/vet-plan.md", "# two\n")]);
        std::fs::write(&flag, b"").unwrap();
        let served = refresh_into(&m, &published, &["vet-plan"], &[]);
        assert_eq!(
            served.map(|s| s.sha),
            Some(one.sha.clone()),
            "the fetch succeeded, the load did not: the previous generation stays"
        );
        assert_eq!(
            published.get().unwrap().commands.bodies["vet-plan"],
            "# one\n"
        );

        std::fs::remove_file(&flag).unwrap();
        let two = refresh_into(&m, &published, &["vet-plan"], &[]).expect("retried");
        assert_ne!(two.sha, one.sha, "the next tick retried the new sha");
        assert_eq!(
            published.get().unwrap().commands.bodies["vet-plan"],
            "# two\n"
        );
    }

    /// A refresh that fails before any load publishes nothing.
    #[test]
    fn a_failed_first_refresh_publishes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mirror(tmp.path(), &tmp.path().join("gone").to_string_lossy());
        let published = Published::new();
        assert_eq!(refresh_into(&m, &published, &["vet-plan"], &[]), None);
        assert!(published.get().is_none());
    }

    /// Another process mid-refresh holds the writer lock: this tick leaves
    /// the mirror alone — its in-flight pack is NOT cleared as a leftover —
    /// and records no failure. Once the lock is free the refresh proceeds.
    #[test]
    fn a_mirror_another_process_is_writing_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let published = Published::new();
        refresh_into(&m, &published, &["vet-plan"], &[]).expect("first load");
        let first = published.snapshot().unwrap();

        // A second handle on the same mirror dir stands in for the other
        // process: the advisory lock is per open file, so it conflicts here.
        let peer = mirror(tmp.path(), &url);
        let held = peer.try_lock_writer().unwrap().expect("the peer takes it");
        let in_flight = tmp.path().join("mirror.git/objects/pack/tmp_pack_Peer01");
        std::fs::write(&in_flight, b"being written").unwrap();
        commit_files(tmp.path(), &[(".claude/commands/vet-plan.md", "# next\n")]);

        assert!(m.try_lock_writer().unwrap().is_none(), "held elsewhere");
        assert!(m.refresh().is_err(), "a bare refresh refuses too");
        let served = refresh_into(&m, &published, &["vet-plan"], &[]);
        assert_eq!(served, Some(first.clone()), "the previous generation stays");
        assert!(in_flight.exists(), "the peer's in-flight pack is untouched");
        assert!(
            published.last_failure.lock().unwrap().is_none(),
            "contention is not a failure"
        );

        assert_eq!(
            published.next_delay(),
            REFRESH_INTERVAL,
            "a process already serving a generation waits the full interval"
        );

        drop(held);
        let next = refresh_into(&m, &published, &["vet-plan"], &[]).expect("refreshed");
        assert_ne!(next.sha, first.sha);
        assert!(!in_flight.exists(), "now a leftover, and cleared");
        assert_eq!(
            published.get().unwrap().commands.bodies["vet-plan"],
            "# next\n"
        );
    }

    /// Losing the race at boot — contended with nothing published — retries
    /// soon; any other tick, including a plain failure, waits the interval.
    #[test]
    fn a_contended_tick_with_nothing_published_retries_soon() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let published = Published::new();
        assert_eq!(published.next_delay(), REFRESH_INTERVAL, "no tick yet");

        let held = mirror(tmp.path(), &url)
            .try_lock_writer()
            .unwrap()
            .expect("the peer takes it");
        assert_eq!(refresh_into(&m, &published, &["vet-plan"], &[]), None);
        assert_eq!(published.next_delay(), CONTENDED_RETRY);
        drop(held);

        refresh_into(&m, &published, &["vet-plan"], &[]).expect("loaded");
        assert_eq!(published.next_delay(), REFRESH_INTERVAL);

        let failed = Published::new();
        let other = tempfile::tempdir().unwrap();
        let gone = mirror(other.path(), &other.path().join("gone").to_string_lossy());
        assert_eq!(refresh_into(&gone, &failed, &["vet-plan"], &[]), None);
        assert_eq!(
            failed.next_delay(),
            REFRESH_INTERVAL,
            "a failure is not contention: no fast retry against a dead URL"
        );
    }

    /// The fetch never starts a detached gc or maintenance run that would
    /// outlive the writer lock.
    #[cfg(unix)]
    #[test]
    fn the_fetch_turns_off_automatic_gc_and_maintenance() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let log = tmp.path().join("argv.log");
        let wrapper = tmp.path().join("logging-git");
        test_support::executable_script(
            &wrapper,
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexec git \"$@\"\n",
                log.display()
            ),
        );
        let m = mirror(tmp.path(), &url).with_program(&wrapper);
        m.refresh().expect("refresh");
        let argv = std::fs::read_to_string(&log).unwrap();
        let fetch = argv
            .lines()
            .find(|l| l.contains(" fetch "))
            .expect("a fetch ran");
        assert!(fetch.contains("-c gc.auto=0"), "{fetch}");
        assert!(fetch.contains("-c maintenance.auto=false"), "{fetch}");
    }

    // -- the rung inside registry resolution ---------------------------------

    use crate::agent_commands::{resolve_with, CommandSource, FetchOutcome};

    fn first_bundled() -> (&'static str, &'static str) {
        crate::fleet_commands::FLEET_COMMANDS[0]
    }

    fn account_override(name: &str, body: &str) -> qontinui_types::agent_commands::AgentCommand {
        qontinui_types::agent_commands::AgentCommand {
            id: "id-1".to_string(),
            organization_id: Some("org-1".to_string()),
            created_by_user_id: None,
            name: name.to_string(),
            body: body.to_string(),
            checksum: None,
            is_shared: false,
            current_version: 1,
            created_at: "2026-09-25T00:00:00Z".to_string(),
            updated_at: "2026-09-25T00:00:00Z".to_string(),
        }
    }

    /// Load the bundled command set from a mirror of a remote holding `files`.
    fn loaded(root: &Path, files: &[(&str, &str)]) -> CanonicalCommands {
        let m = mirror(root, &remote_with(root, files));
        let snap = m.refresh().expect("refresh");
        commands_at(&m, &snap, &bundled_command_names())
    }

    /// The embedded floor, byte-identically — what every failure arm must
    /// resolve to.
    fn assert_all_embedded(registry: &crate::agent_commands::AgentCommandRegistry) {
        for (name, body) in crate::fleet_commands::FLEET_COMMANDS {
            let c = registry.get(name).expect("bundled");
            assert_eq!(c.source, CommandSource::Builtin, "{name}");
            assert_eq!(&c.body, body, "{name}");
            assert_eq!(c.canonical, None);
        }
    }

    /// A canonical body that differs from the embedded one wins over the
    /// embedded default, carries its snapshot, and loses to an account override.
    #[test]
    fn canonical_beats_embedded_and_loses_to_an_account_override() {
        let tmp = tempfile::tempdir().unwrap();
        let (first, embedded) = first_bundled();
        let path = format!(".claude/commands/{first}.md");
        let canonical = loaded(tmp.path(), &[(&path, "# canonical body\n")]);
        assert_ne!(embedded, "# canonical body\n");

        let (registry, _) = resolve_with(FetchOutcome::NoAccount, None, Some(&canonical));
        let c = registry.get(first).unwrap();
        assert_eq!(c.source, CommandSource::Canonical);
        assert_eq!(c.body, "# canonical body\n");
        assert_eq!(c.canonical.as_ref(), Some(&canonical.snapshot));
        assert_eq!(registry.canonical_count(), 1);
        // The account layer still reports its own arm; canonical is per-body.
        assert_eq!(registry.resolution_arm(), CommandSource::Builtin);
        // Commands the canonical repo lacks stay embedded.
        let (second, second_body) = crate::fleet_commands::FLEET_COMMANDS[1];
        assert_eq!(registry.get(second).unwrap().source, CommandSource::Builtin);
        assert_eq!(registry.get(second).unwrap().body, second_body);

        let (registry, _) = resolve_with(
            FetchOutcome::Fresh(vec![account_override(first, "# mine\n")]),
            None,
            Some(&canonical),
        );
        let c = registry.get(first).unwrap();
        assert_eq!(c.source, CommandSource::Served);
        assert_eq!(c.body, "# mine\n");
        assert_eq!(registry.canonical_count(), 0);
        assert_eq!(
            registry.all().len(),
            crate::fleet_commands::FLEET_COMMANDS.len()
        );
    }

    /// The canonical repo holds commands this binary does not bundle; the rung
    /// refreshes the bundle, it never widens it.
    #[test]
    fn a_canonical_command_the_bundle_lacks_is_not_added() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mirror(
            tmp.path(),
            &remote_with(tmp.path(), &[(".claude/commands/not-bundled.md", "# x\n")]),
        );
        let snap = m.refresh().unwrap();
        let mut bodies = BTreeMap::new();
        bodies.insert("not-bundled".to_string(), "# x\n".to_string());
        let canonical = CanonicalCommands {
            snapshot: snap,
            bodies,
        };
        let (registry, _) = resolve_with(FetchOutcome::NoAccount, None, Some(&canonical));
        assert!(registry.get("not-bundled").is_none());
        assert_all_embedded(&registry);
    }

    /// A canonical body that fails the account layer's validation falls to the
    /// embedded default.
    #[test]
    fn a_malformed_canonical_body_falls_to_the_embedded_default() {
        let tmp = tempfile::tempdir().unwrap();
        let (first, _) = first_bundled();
        let path = format!(".claude/commands/{first}.md");
        let canonical = loaded(tmp.path(), &[(&path, "   \n")]);
        assert_eq!(canonical.bodies.len(), 1, "read, but not yet validated");
        let (registry, _) = resolve_with(FetchOutcome::NoAccount, None, Some(&canonical));
        assert_all_embedded(&registry);
    }

    /// A sha that lacks every bundled path loads nothing; resolution is the
    /// embedded floor.
    #[test]
    fn a_sha_without_the_paths_falls_to_the_embedded_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = loaded(tmp.path(), &[("README.md", "# nothing here\n")]);
        assert!(canonical.bodies.is_empty());
        let (registry, _) = resolve_with(FetchOutcome::NoAccount, None, Some(&canonical));
        assert_all_embedded(&registry);
    }

    /// Unreachable remote and a missing git: the refresh fails, no snapshot
    /// exists, and resolution with none is the embedded floor.
    #[test]
    fn a_failed_refresh_leaves_resolution_on_the_embedded_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let unreachable = mirror(tmp.path(), &tmp.path().join("gone").to_string_lossy());
        assert!(unreachable.refresh().is_err());
        let (registry, _) = resolve_with(FetchOutcome::NoAccount, None, None);
        assert_all_embedded(&registry);
    }

    // -- skills --------------------------------------------------------------

    /// `list_tree` yields every file under the directory with git's recorded
    /// mode — including an extension-less executable — and a skill loads
    /// with those modes.
    #[test]
    fn list_tree_yields_paths_and_git_modes() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(
            tmp.path(),
            &[
                (
                    ".claude/skills/demo/SKILL.md",
                    "---\nname: demo\n---\n# demo\n",
                ),
                (".claude/skills/demo/helper", "#!/bin/sh\necho hi\n"),
                (".claude/skills/demo/ref/notes.md", "# notes\n"),
                (".claude/skills/other/SKILL.md", "# other\n"),
            ],
        );
        test_support::chmod_x(tmp.path(), ".claude/skills/demo/helper");
        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().unwrap();
        let mut entries: Vec<(String, u32)> = m
            .list_tree(&snap, &[".claude/skills/demo"])
            .unwrap()
            .into_iter()
            .map(|e| (e.path, e.mode))
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                (".claude/skills/demo/SKILL.md".to_string(), MODE_FILE),
                (".claude/skills/demo/helper".to_string(), MODE_EXECUTABLE),
                (".claude/skills/demo/ref/notes.md".to_string(), MODE_FILE),
            ]
        );
        assert!(m
            .list_tree(&snap, &[".claude/skills/absent"])
            .unwrap()
            .is_empty());

        let loaded = m.load(&snap, &[], &["demo", "absent"]).unwrap().skills;
        assert_eq!(loaded.skills.len(), 1);
        let demo = &loaded.skills["demo"];
        assert!(demo.is_executable("helper"));
        assert!(!demo.is_executable("SKILL.md"));
        assert_eq!(demo.files["ref/notes.md"], "# notes\n");
    }

    /// A skill whose files fail the account layer's validation (here: no
    /// `SKILL.md`) is refused whole.
    #[test]
    fn an_invalid_canonical_skill_is_refused_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(
            tmp.path(),
            &[(".claude/skills/demo/README.md", "# no manifest\n")],
        );
        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().unwrap();
        assert!(m
            .load(&snap, &[], &["demo"])
            .unwrap()
            .skills
            .skills
            .is_empty());
    }

    /// A git that hangs is killed at the budget, and the whole refresh returns
    /// promptly with a timeout reason.
    #[cfg(unix)]
    #[test]
    fn a_hanging_git_times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("sleepy-git");
        test_support::executable_script(&script, "#!/bin/sh\nexec sleep 30\n");
        let budget = Duration::from_millis(200);
        let m = mirror(tmp.path(), "unused")
            .with_program(&script)
            .with_timeouts(budget, budget);
        let started = std::time::Instant::now();
        let err = m.refresh().expect_err("hangs");
        assert!(err.contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a hung git must not hold the refresh past its budget: {:?}",
            started.elapsed()
        );
    }
}

/// The registered flow test for the skill mirror (`crate::egress::FLOW_TESTS`):
/// [`refresh_into`] against a counting loopback "git host", with the switch
/// pinned each way.
#[cfg(test)]
pub(crate) mod egress_tests {
    use super::*;
    use crate::egress::test_support::{pin, ConnCounter};
    use crate::egress::{Flow, Level};

    fn refresh_once(level: Level, published: &Published) -> usize {
        let dir = tempfile::tempdir().unwrap();
        let counter = ConnCounter::start();
        let mirror = Mirror::new(
            dir.path().join("mirror.git"),
            format!("{}/example/corpus.git", counter.http_base()),
        )
        .with_timeouts(Duration::from_secs(20), Duration::from_secs(20));
        let _pin = pin(Flow::SkillMirror, level);
        let _ = refresh_into(&mirror, published, &[], &[]);
        counter.wait_for(1, Duration::from_millis(300))
    }

    #[test]
    pub(crate) fn skill_mirror_pinned_off_makes_zero_connections() {
        let published = Published::new();
        assert_eq!(
            refresh_once(Level::Off, &published),
            0,
            "no fetch with the switch off"
        );
        assert!(published.get().is_none(), "the embedded floor is served");
    }

    #[test]
    pub(crate) fn skill_mirror_pinned_on_makes_a_connection() {
        let published = Published::new();
        assert!(
            refresh_once(Level::On, &published) >= 1,
            "with the switch on git fetches"
        );
    }

    /// A generation fetched before the flip is not served after it.
    #[test]
    fn a_flip_to_off_unpublishes_an_earlier_generation() {
        let root = tempfile::tempdir().unwrap();
        let url = test_support::remote_with(root.path(), &[("README.md", "synthetic")]);
        let mirror = Mirror::new(root.path().join("mirror.git"), url);
        let published = Published::new();
        let _ = refresh_into(&mirror, &published, &[], &[]);
        assert!(
            published.get().is_some(),
            "precondition: a generation is published"
        );
        let _pin = pin(Flow::SkillMirror, Level::Off);
        assert!(refresh_into(&mirror, &published, &[], &[]).is_none());
        assert!(published.get().is_none());
        // The mirror's own fetch refuses too.
        let held = mirror.try_lock_writer().unwrap().unwrap();
        assert_eq!(mirror.fetch(&held).unwrap_err(), SKILL_MIRROR_OFF);
    }
}
