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
//! [`refresh`] is ONE bounded `git fetch <url> +refs/heads/main:refs/remotes/origin/main`
//! ([`FETCH_TIMEOUT`], the 20 s class `git_trunk` uses for its periodic reads),
//! followed by local plumbing reads that load every bundled command's body at
//! the fetched sha into memory. It runs on a background timer
//! ([`start_refresh_loop`], [`REFRESH_INTERVAL`]), never on a spawn. Registry
//! resolution then reads the last loaded snapshot synchronously through
//! [`latest`] — a lock and an `Arc` clone, no I/O — so the first spawn after
//! boot may see no snapshot and fall to the embedded default, which its
//! provenance key labels `source=builtin`.
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
//! Every body read is addressed by the FULL sha (`<sha>:<path>`), never by a
//! slash-ref: finding `89257638` records MSYS silently mangling
//! `origin/main:.claude/...`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tracing::{info, warn};

use crate::process_helpers::{TimedOutput, TimedRun};

/// Budget for the one network operation, `git fetch`. The same 20 s class
/// `git_trunk::TRUNK_GIT_TIMEOUT` uses for a periodic, off-spawn-path read.
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for each LOCAL plumbing read against the mirror (`init`,
/// `rev-parse`, `cat-file`). Milliseconds when healthy; the only realistic hang
/// is a lock or a stalled filesystem.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);

/// How often [`start_refresh_loop`] fetches.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

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

/// Which `origin/main` generation a body was read at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalSnapshot {
    /// The FULL 40-hex commit sha `refs/remotes/origin/main` resolved to.
    pub sha: String,
    /// RFC 3339 time of the fetch that produced it.
    pub fetched_at: String,
}

impl CanonicalSnapshot {
    /// The first 12 hex digits — the `canonical_sha=` provenance field.
    pub fn short(&self) -> &str {
        self.sha.get(..12).unwrap_or(&self.sha)
    }
}

/// The bundled commands' bodies as `qontinui-claude-config` holds them at one
/// snapshot. A name absent from `bodies` was absent (or unreadable) at that
/// sha and falls to the next rung.
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

/// One file of a `git ls-tree -r` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Path RELATIVE to the listed directory, `/`-separated.
    pub path: String,
    /// The mode git recorded (`0o100644`, `0o100755`, `0o120000`, …).
    pub mode: u32,
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
    local_timeout: Duration,
}

impl Mirror {
    pub(crate) fn new(git_dir: PathBuf, url: String) -> Self {
        Self {
            git_dir,
            url,
            program: OsString::from("git"),
            fetch_timeout: FETCH_TIMEOUT,
            local_timeout: LOCAL_TIMEOUT,
        }
    }

    /// Replace the git program — a nonexistent path or a sleeping script.
    #[cfg(test)]
    pub(crate) fn with_program(mut self, program: impl Into<OsString>) -> Self {
        self.program = program.into();
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timeouts(mut self, fetch: Duration, local: Duration) -> Self {
        self.fetch_timeout = fetch;
        self.local_timeout = local;
        self
    }

    /// A git command with the caller's repository environment scrubbed, so an
    /// inherited `GIT_DIR` / `GIT_WORK_TREE` can never redirect a read or a
    /// fetch at some other repository. `no_window` also applies the
    /// prompt-proof git posture, so a credential prompt fails instead of
    /// blocking.
    fn command(&self) -> std::process::Command {
        let mut cmd = crate::process_helpers::no_window(&self.program);
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    /// A git command addressed at the mirror.
    fn in_mirror(&self) -> std::process::Command {
        let mut cmd = self.command();
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
        let mut cmd = self.command();
        cmd.args(["init", "--bare", "--quiet"]).arg(&self.git_dir);
        run(cmd, self.local_timeout, "git init --bare").map(|_| ())
    }

    /// Fetch `main` from the URL into [`TRACKING_REF`] and resolve it to a
    /// full sha. One network operation, bounded by the fetch budget.
    pub(crate) fn refresh(&self) -> Result<CanonicalSnapshot, String> {
        self.ensure_init()?;
        let mut fetch = self.in_mirror();
        fetch
            .args(["fetch", "--quiet", "--no-tags", "--no-write-fetch-head"])
            .arg(&self.url)
            .arg(format!("+refs/heads/main:{TRACKING_REF}"));
        run(fetch, self.fetch_timeout, "git fetch")?;

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

    /// The bytes of `rel_path` at `snapshot` — `git cat-file blob <sha>:<path>`,
    /// addressed by the FULL sha. A path absent at that sha, or one naming a
    /// tree rather than a file, is an error.
    pub(crate) fn read(
        &self,
        snapshot: &CanonicalSnapshot,
        rel_path: &str,
    ) -> Result<Vec<u8>, String> {
        let mut cmd = self.in_mirror();
        cmd.args(["cat-file", "blob"])
            .arg(format!("{}:{rel_path}", snapshot.sha));
        run(cmd, self.local_timeout, "git cat-file")
    }

    /// Every file under `path` at `snapshot` with the mode git recorded —
    /// `git ls-tree -r -z <sha> -- <path>`, blobs only, paths made relative to
    /// `path`. A path absent at that sha lists nothing and is an error.
    pub(crate) fn list_tree(
        &self,
        snapshot: &CanonicalSnapshot,
        path: &str,
    ) -> Result<Vec<TreeEntry>, String> {
        let mut cmd = self.in_mirror();
        cmd.args(["ls-tree", "-r", "-z", "--full-tree"])
            .arg(&snapshot.sha)
            .arg("--")
            .arg(path);
        let out = run(cmd, self.local_timeout, "git ls-tree")?;
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let mut entries = Vec::new();
        for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let record = String::from_utf8_lossy(record);
            // `<mode> SP <type> SP <object> TAB <path>`
            let (meta, full_path) = record
                .split_once('\t')
                .ok_or_else(|| format!("git ls-tree: malformed record {record:?}"))?;
            let mut fields = meta.split(' ');
            let (Some(mode), Some(kind)) = (fields.next(), fields.next()) else {
                return Err(format!("git ls-tree: malformed record {record:?}"));
            };
            if kind != "blob" {
                continue;
            }
            let mode = u32::from_str_radix(mode, 8)
                .map_err(|_| format!("git ls-tree: malformed mode {mode:?}"))?;
            let Some(rel) = full_path.strip_prefix(&prefix) else {
                continue;
            };
            entries.push(TreeEntry {
                path: rel.to_string(),
                mode,
            });
        }
        if entries.is_empty() {
            return Err(format!("{path} is absent at {}", snapshot.short()));
        }
        Ok(entries)
    }

    /// Load one skill directory, `.claude/skills/<name>/`, at `snapshot`, and
    /// run it through the same validation an account skill passes. Any file
    /// that is not a regular or executable blob (a symlink, a submodule), any
    /// unreadable or non-UTF-8 file, and any validation failure refuses the
    /// WHOLE skill — a half-canonical skill is a `SKILL.md` citing files that
    /// are not there.
    fn load_skill(
        &self,
        snapshot: &CanonicalSnapshot,
        name: &str,
    ) -> Result<CanonicalSkill, String> {
        let dir = format!(".claude/skills/{name}");
        let mut files = qontinui_types::agent_text_units::AgentTextUnitFiles::new();
        let mut modes = BTreeMap::new();
        for entry in self.list_tree(snapshot, &dir)? {
            if entry.mode != MODE_FILE && entry.mode != MODE_EXECUTABLE {
                return Err(format!("{} has git mode {:o}", entry.path, entry.mode));
            }
            let bytes = self.read(snapshot, &format!("{dir}/{}", entry.path))?;
            let text =
                String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8", entry.path))?;
            modes.insert(entry.path.clone(), entry.mode);
            files.insert(entry.path, text);
        }
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
            source_path: Some(dir),
            source_commit: Some(snapshot.sha.clone()),
            created_at: snapshot.fetched_at.clone(),
            updated_at: snapshot.fetched_at.clone(),
        };
        crate::agent_skills::validate_override(
            &unit,
            crate::agent_skills::AgentSkillSource::Builtin,
        )?;
        Ok(CanonicalSkill { files, modes })
    }

    /// Load every skill in `names` at `snapshot`. A skill that is absent or
    /// refused is left out — it falls to the next rung — and the misses are
    /// logged as ONE line.
    pub(crate) fn load_skills(
        &self,
        snapshot: &CanonicalSnapshot,
        names: &[&str],
    ) -> CanonicalSkills {
        let mut skills = BTreeMap::new();
        let mut missed: Vec<String> = Vec::new();
        for name in names {
            match self.load_skill(snapshot, name) {
                Ok(skill) => {
                    skills.insert((*name).to_string(), skill);
                }
                Err(why) => missed.push(format!("{name} ({why})")),
            }
        }
        if !missed.is_empty() {
            warn!(
                "canonical_corpus: {} of {} bundled skill(s) unusable at {} — they fall to \
                 the embedded default: {}",
                missed.len(),
                names.len(),
                snapshot.short(),
                missed.join("; ")
            );
        }
        CanonicalSkills {
            snapshot: snapshot.clone(),
            skills,
        }
    }

    /// Load every name in `names` from `.claude/commands/<name>.md` at
    /// `snapshot`. A body that is absent, unreadable, or not UTF-8 is left
    /// out — that command falls to the next rung — and the misses are logged
    /// as ONE line, not one per command.
    pub(crate) fn load_commands(
        &self,
        snapshot: &CanonicalSnapshot,
        names: &[&str],
    ) -> CanonicalCommands {
        let mut bodies = BTreeMap::new();
        let mut missed: Vec<String> = Vec::new();
        for name in names {
            let path = format!(".claude/commands/{name}.md");
            match self.read(snapshot, &path) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(body) => {
                        bodies.insert((*name).to_string(), body);
                    }
                    Err(_) => missed.push(format!("{name} (not UTF-8)")),
                },
                Err(why) => missed.push(format!("{name} ({why})")),
            }
        }
        if !missed.is_empty() {
            warn!(
                "canonical_corpus: {} of {} bundled command(s) unreadable at {} — they fall \
                 to the embedded default: {}",
                missed.len(),
                names.len(),
                snapshot.short(),
                missed.join("; ")
            );
        }
        CanonicalCommands {
            snapshot: snapshot.clone(),
            bodies,
        }
    }
}

/// Run `cmd` under `timeout` through the crate's bounded runner, returning its
/// stdout only when it exited 0 AND its output was read in full. `what` is a
/// fixed, caller-authored label — never the argv, which may carry a URL with
/// a credential in it.
fn run(cmd: std::process::Command, timeout: Duration, what: &str) -> Result<Vec<u8>, String> {
    match crate::process_helpers::run_with_timeout_detailed(cmd, timeout) {
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

/// The last successfully loaded corpus. `None` until the first refresh lands.
static LATEST: RwLock<Option<Arc<CanonicalCorpus>>> = RwLock::new(None);

/// The last refresh failure reason, so a failure repeating on every timer tick
/// is logged once rather than every fifteen minutes forever.
static LAST_FAILURE: Mutex<Option<String>> = Mutex::new(None);

/// The last loaded corpus, for synchronous registry resolution. No I/O.
pub(crate) fn latest() -> Option<Arc<CanonicalCorpus>> {
    LATEST
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
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
        let mut cmd = crate::process_helpers::no_window("git");
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

/// Refresh `mirror` and, when the fetched sha differs from what is loaded,
/// load the bundled bodies at it and publish them. Returns the snapshot the
/// process now serves (`None` when nothing has ever loaded).
///
/// A failure keeps the previously published corpus: a stale canonical copy is
/// still the newest one this device can read.
pub(crate) fn refresh_into_latest(mirror: &Mirror) -> Option<CanonicalSnapshot> {
    match mirror.refresh() {
        Ok(snapshot) => {
            {
                let mut last = LAST_FAILURE
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(prev) = last.take() {
                    info!("canonical_corpus: refresh recovered (previously: {prev})");
                }
            }
            if let Some(current) = latest() {
                if current.snapshot().sha == snapshot.sha {
                    return Some(current.snapshot().clone());
                }
            }
            let skill_names = bundled_skill_names();
            let skill_refs: Vec<&str> = skill_names.iter().map(String::as_str).collect();
            let corpus = CanonicalCorpus {
                commands: mirror.load_commands(&snapshot, &bundled_command_names()),
                skills: mirror.load_skills(&snapshot, &skill_refs),
            };
            info!(
                "canonical_corpus: serving qontinui-claude-config@{} ({} of {} bundled \
                 command(s), {} of {} bundled skill(s) present)",
                snapshot.short(),
                corpus.commands.bodies.len(),
                crate::fleet_commands::FLEET_COMMANDS.len(),
                corpus.skills.skills.len(),
                skill_names.len(),
            );
            *LATEST
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(corpus));
            Some(snapshot)
        }
        Err(why) => {
            let mut last = LAST_FAILURE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if last.as_deref() != Some(why.as_str()) {
                warn!(
                    "canonical_corpus: refresh failed ({why}) — sessions keep the {} and fall \
                     back to the embedded defaults for anything it lacks",
                    match latest() {
                        Some(c) => format!("previously loaded snapshot {}", c.snapshot().short()),
                        None => "embedded defaults".to_string(),
                    }
                );
                *last = Some(why);
            }
            latest().map(|c| c.snapshot().clone())
        }
    }
}

/// One refresh of the production mirror. Blocking — call it off the async
/// runtime's worker threads.
pub(crate) fn refresh() -> Option<CanonicalSnapshot> {
    let mirror = default_mirror()?;
    refresh_into_latest(&mirror)
}

/// Whether [`start_refresh_loop`] has already started this process's loop.
static LOOP_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Start the background refresher: a short boot grace, then a refresh every
/// [`REFRESH_INTERVAL`], each on the blocking pool. Hung beside the embedded
/// defaults publisher in `mcp_api::create_router`, never on a spawn path. Once
/// per process: a second call (a second router) is a no-op, so the mirror is
/// never fetched by two loops at once.
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
            tokio::time::sleep(REFRESH_INTERVAL).await;
        }
    });
}

/// Test fixtures shared with the resolvers' tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::{Command, Stdio};

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} should succeed");
    }

    /// A local "remote": a non-bare repo on branch `main` holding `files`
    /// (relative path → contents), one commit. Returns its path as the URL.
    pub(crate) fn remote_with(root: &Path, files: &[(&str, &str)]) -> String {
        let remote = root.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--quiet", "--initial-branch=main"]);
        git(&remote, &["config", "user.email", "t@example.com"]);
        git(&remote, &["config", "user.name", "t"]);
        for (path, text) in files {
            let dst = remote.join(path);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::write(&dst, text).unwrap();
        }
        git(&remote, &["add", "--all"]);
        git(&remote, &["commit", "--quiet", "-m", "fixture"]);
        remote.to_string_lossy().into_owned()
    }

    /// Mark `path` executable in the remote's index and commit it, so git
    /// records mode `100755` regardless of the filesystem's own bits.
    pub(crate) fn chmod_x(root: &Path, path: &str) {
        let remote = root.join("remote");
        git(&remote, &["update-index", "--chmod=+x", "--", path]);
        git(&remote, &["commit", "--quiet", "-m", "chmod"]);
    }

    /// A mirror under `root` fetching `url`.
    pub(crate) fn mirror(root: &Path, url: &str) -> Mirror {
        Mirror::new(root.join("mirror.git"), url.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::test_support::{mirror, remote_with};
    use super::*;

    #[test]
    fn refresh_resolves_the_full_sha_and_reads_by_it() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().expect("refresh");
        assert_eq!(snap.sha.len(), 40);
        assert_eq!(snap.short().len(), 12);
        assert_eq!(
            m.read(&snap, ".claude/commands/vet-plan.md").unwrap(),
            b"# canon\n"
        );
        // A second refresh over the initialised mirror is idempotent.
        assert_eq!(m.refresh().unwrap().sha, snap.sha);
        // The mirror is bare and runner-owned: no working tree was created.
        assert!(tmp.path().join("mirror.git").join("HEAD").is_file());
        assert!(!tmp.path().join("mirror.git").join(".claude").exists());
    }

    #[test]
    fn a_path_absent_at_the_sha_is_an_error_and_is_left_out() {
        let tmp = tempfile::tempdir().unwrap();
        let url = remote_with(tmp.path(), &[(".claude/commands/vet-plan.md", "# canon\n")]);
        let m = mirror(tmp.path(), &url);
        let snap = m.refresh().unwrap();
        assert!(m.read(&snap, ".claude/commands/absent.md").is_err());
        // A directory is not a file.
        assert!(m.read(&snap, ".claude/commands").is_err());
        let loaded = m.load_commands(&snap, &["vet-plan", "absent"]);
        assert_eq!(loaded.bodies.len(), 1);
        assert_eq!(loaded.bodies["vet-plan"], "# canon\n");
        assert_eq!(loaded.snapshot, snap);
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
        m.load_commands(&snap, &bundled_command_names())
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

    /// `list_tree` yields every file under the directory, relative to it, with
    /// git's recorded mode — including an extension-less executable.
    #[test]
    fn list_tree_yields_relative_paths_and_git_modes() {
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
        let mut entries = m.list_tree(&snap, ".claude/skills/demo").unwrap();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(
            entries,
            vec![
                TreeEntry {
                    path: "SKILL.md".into(),
                    mode: MODE_FILE
                },
                TreeEntry {
                    path: "helper".into(),
                    mode: MODE_EXECUTABLE
                },
                TreeEntry {
                    path: "ref/notes.md".into(),
                    mode: MODE_FILE
                },
            ]
        );
        assert!(m.list_tree(&snap, ".claude/skills/absent").is_err());

        let loaded = m.load_skills(&snap, &["demo", "absent"]);
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
        assert!(m.load_skills(&snap, &["demo"]).skills.is_empty());
    }

    /// A git that hangs is killed at the budget, and the whole refresh returns
    /// promptly with a timeout reason.
    #[cfg(unix)]
    #[test]
    fn a_hanging_git_times_out() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("sleepy-git");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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
