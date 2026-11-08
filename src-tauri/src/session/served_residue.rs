//! The served-corpus residue rule: may a finished session's tree close when
//! the only untracked files in it are the runner's own provisioned copies?
//!
//! The runner writes its served corpus — `.claude/commands/**`,
//! `.claude/skills/**`, and on the agent-dispatch path `.claude/agents/**` —
//! into every session cwd at spawn. None of it is the session's work, and none
//! of it exists only on that disk. Before this module, the custody probe
//! ([`super::wind_down_observer::GitCustodyProbe`]) marked a tree dirty on any
//! `git status` output, so `finished_close` refused every no-op closeout in a
//! repo that does not ignore `.claude/` (plan
//! `2026-11-03-the-runners-served-claude-corpus-makes-every-no-op-closeout-not-closeable`,
//! Phase 2, runner half).
//!
//! The property tested is "these exact bytes are the runner's own and exist
//! outside this tree, in a provisioner source" — never a path pattern, which
//! would pass a session-authored `.claude/commands/x.md`
//! [policy: verification-and-evidence/a-control-must-test-the-property-it-names].
//!
//! **The reference implementation is the shell half**, qontinui-claude-config
//! `scripts/session-closeable.sh` (`served_corpus_classify` / `judge_changes`),
//! and this module mirrors its R/U/K/X mapping — plan items 1-12 as amended —
//! verdict for verdict:
//!
//! * `R` residue: the runner's own bytes.
//! * `U` uncommitted: a definite reason the tree holds work.
//! * `K` unknown: cannot be checked here; never a pass, never a definite reason.
//! * `X` left out: bytecode of a TRACKED `.py`, judged through its source.
//!
//! The per-file rules, by item number in the plan:
//!
//! 1. Scope: `git ls-files --others` over the three served roots WITHOUT
//!    `--exclude-standard`, so ignored and excluded copies are judged too.
//! 2. A stamped file is residue only when its stamp parses in the runner's
//!    closed grammar, names this path, and the stripped body hashes to its own
//!    `blob=` ([`crate::provenance::provenance_consistent`]); otherwise `U`.
//! 3. An unstamped file in a skill takes its `SKILL.md`'s stamp: `canonical`
//!    → its blob must equal qontinui-claude-config at `canonical_sha`;
//!    `builtin` → the runner checkout at the build's sha. A mismatch is `U`.
//! 4. An unstamped agent def equal to qontinui-claude-config `origin/main`, its
//!    checkout's copy, or a version `origin/main` held there and replaced, is
//!    residue; otherwise `K` (both references move).
//! 5. `__pycache__/` bytecode: beside a residue source, residue only when it is
//!    this interpreter's exact bytecode of it; beside a tracked source, left
//!    out (`X`) when it is a plausible cache. Checked by
//!    `served_residue_pyc.py`, a verbatim copy of the script's `pyc_*`
//!    functions, run under `python3 -I`.
//! 6. An account source, a missing object, no reference checkout → `K`.
//! 7. An unstamped `.claude/commands/*` → `U`.
//! 8. A path no provisioner's source history ever held → `U` (both histories
//!    read; a shallow one is named in the reason).
//! 9. A symlink → `U`.
//! 10. Any stderr from a `.claude/` listing leaves what it hid unjudged → `K`.
//! 11. An embedded repository → `U`.
//! 12. Any stderr from `git status` → `K` likewise; a definite reason still
//!     outranks both.
//!
//! Caller `GIT_CONFIG_*` overrides are dropped before every probe (one can set
//! `core.excludesFile` and hide untracked work), and files git ignores outside
//! `.claude/` are not judged — both as in the script.
//!
//! The shell-only hardening (trust boundary, completion check, coord-door
//! rule) has no counterpart here: this is Rust over git's own output, and an
//! incomplete classification is a `K` by construction.
//!
//! One deliberate difference: a stamped file whose bytes are not UTF-8 is `U`
//! here (the runner writes every stamped file from a `String`, so no runner
//! wrote it), where the script would hash its bytes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use regex::Regex;

/// The three folders the runner provisions into a session cwd.
pub(crate) const SERVED_ROOTS: [&str; 3] = [".claude/commands", ".claude/skills", ".claude/agents"];
const SERVED_LABEL: &str = ".claude/{commands,skills,agents}";
/// How many paths a reason lists before it says how many more there are.
const SERVED_LIST_MAX: usize = 10;

const CCFG_SKILLS: &str = ".claude/skills";
const RUNNER_SKILLS: &str = "src-tauri/src/fleet_skills";
const CCFG_AGENTS: &str = ".claude/agents";
const RUNNER_AGENTS: &str = "src-tauri/src/fleet_agents";
const NEVER: &str = "no runner provisioner source (the qontinui-claude-config or qontinui-runner history) has ever held this path";

/// `served_residue_pyc.py`, run with `python3 -I -c`.
const PYC_CHECK: &str = include_str!("served_residue_pyc.py");

static PYC: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^((?:[^/]+/)*)__pycache__/([^/.]+)\.([a-z]+-?[0-9]+)(?:\.opt-([12]))?\.pyc$")
        .expect("static regex")
});
static SKILL_FILE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\.claude/skills/[^/]+/.+$").expect("static regex"));
static BUILD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:unstamped-([0-9a-f]{4,40})|([0-9a-f]{4,40})-[0-9]+)$").expect("static regex")
});
static CANONICAL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^qontinui-claude-config:\.claude/(?:commands/[A-Za-z0-9._-]+\.md|skills/[A-Za-z0-9._-]+/SKILL\.md)$",
    )
    .expect("static regex")
});
static BLOB: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[0-9a-f]{40}$").expect("static regex"));
static SHA12: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[0-9a-f]{12}$").expect("static regex"));

/// The reference checkouts the rule reads provisioner sources from.
#[derive(Debug, Clone, Default)]
pub(crate) struct ServedRefs {
    /// The qontinui-claude-config checkout (the agent overlay's source, and the
    /// repository a `source=canonical` stamp's `canonical_sha` is read in).
    pub ccfg: Option<PathBuf>,
    /// The qontinui-runner checkout (where a builtin stamp's build sha is read).
    pub runner: Option<PathBuf>,
}

impl ServedRefs {
    /// The two checkouts under a workspace root, as the fleet lays them out.
    pub(crate) fn under(workspace_root: Option<&Path>) -> Self {
        Self {
            ccfg: workspace_root.map(|w| w.join("qontinui-claude-config")),
            runner: workspace_root.map(|w| w.join("qontinui-runner")),
        }
    }
}

/// One file's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// `R`: the runner's own bytes.
    Residue,
    /// `U`: a definite reason this tree holds work.
    Uncommitted(String),
    /// `K`: could not be checked here.
    Unknown(String),
    /// `X`: bytecode of a tracked `.py`; neither residue nor a reason.
    LeftOut,
}

/// What judging one tree's untracked changes found — `judge_changes` in the
/// script. Definite reasons outrank unknowns; neither closes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChangesJudgement {
    /// Definite reasons the tree holds work, each a whole sentence.
    pub uncommitted: Option<String>,
    /// Why what the tree holds could not be judged, when nothing definite was
    /// found.
    pub unknown: Option<String>,
    /// How many untracked served files were judged residue.
    pub residue: usize,
}

impl ChangesJudgement {
    /// Nothing definite and nothing unjudged: the tree holds no work of its own.
    pub(crate) fn is_clean(&self) -> bool {
        self.uncommitted.is_none() && self.unknown.is_none()
    }
}

/// A bounded git runner for the rule: `-C <cwd>`, the probe environment, and
/// one overall deadline shared by every call in a pass.
pub(crate) struct Git {
    per_call: Duration,
    deadline: Instant,
}

/// What one git call returned.
pub(crate) struct GitOut {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl GitOut {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl Git {
    /// Each call is bounded by `per_call`; the whole pass by four of them,
    /// the script's bound on its classifier.
    pub(crate) fn new(per_call: Duration) -> Self {
        Self {
            per_call,
            deadline: Instant::now() + per_call * 4,
        }
    }

    /// The probe environment every call gets: no optional locks (the probe runs
    /// in peers' worktrees), no lazy fetch, no prompt, no replace refs, no
    /// inherited repository, and no caller `GIT_CONFIG_*` override.
    pub(crate) fn scrub(cmd: &mut std::process::Command) {
        cmd.env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_CONFIG") {
                cmd.env_remove(key);
            }
        }
    }

    /// Run git in `cwd`. `None` when it could not run, `cwd` is not a
    /// directory, or the call (or the pass) ran out of time.
    pub(crate) fn run(&self, cwd: &Path, args: &[&str]) -> Option<GitOut> {
        if !cwd.is_dir() {
            return None;
        }
        let now = Instant::now();
        if now >= self.deadline {
            return None;
        }
        let bound = self.per_call.min(self.deadline - now);
        let out = tempfile::NamedTempFile::new().ok()?;
        let err = tempfile::NamedTempFile::new().ok()?;
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("--no-optional-locks").arg("-C").arg(cwd).args(args);
        Self::scrub(&mut cmd);
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::from(out.reopen().ok()?))
            .stderr(Stdio::from(err.reopen().ok()?))
            .spawn()
            .ok()?;
        let until = Instant::now() + bound;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= until => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return None,
            }
        };
        Some(GitOut {
            code: status.code(),
            stdout: std::fs::read(out.path()).ok()?,
            stderr: std::fs::read(err.path()).ok()?,
        })
    }

    /// git's stdout on a zero exit, else `None`.
    fn stdout(&self, cwd: &Path, args: &[&str]) -> Option<Vec<u8>> {
        self.run(cwd, args).filter(GitOut::ok).map(|o| o.stdout)
    }
}

/// The first non-blank line of git's stderr, bounded, for a reason.
pub(crate) fn stderr_gist(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("stderr holding only blank lines");
    line.chars().take(200).collect()
}

fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// The stamp, in the runner's closed grammar (provenance_stamp.py's twin)
// ---------------------------------------------------------------------------

/// Line 1 is a frontmatter opener and line 2 starts with the key — whether or
/// not the value parses (`has_provenance_key`).
fn has_provenance_key(raw: &[u8]) -> bool {
    let key = crate::provenance::PROVENANCE_KEY.as_bytes();
    let first_end = match raw.iter().position(|b| *b == b'\n') {
        Some(i) => i + 1,
        None => return false,
    };
    let first = &raw[..first_end];
    (first == b"---\n" || first == b"---\r\n") && raw[first_end..].starts_with(key)
}

/// The stamp's fields when the value parses in the grammar a runner renders:
/// the closed source vocabulary, a command or SKILL.md canonical path, a
/// 40-hex blob, and `canonical_sha=<sha12>` exactly when `source=canonical`
/// (`parse_provenance_value`).
fn stamp_fields(raw: &[u8]) -> Option<HashMap<String, String>> {
    if !has_provenance_key(raw) {
        return None;
    }
    let start = raw.iter().position(|b| *b == b'\n')? + 1;
    let rest = &raw[start..];
    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
    let mut line = &rest[..end];
    if line.ends_with(b"\r") && end < rest.len() {
        line = &line[..line.len() - 1];
    }
    let value = std::str::from_utf8(&line[crate::provenance::PROVENANCE_KEY.len()..]).ok()?;
    if !value.is_ascii() {
        return None;
    }
    let mut fields = HashMap::new();
    for token in value.split_whitespace() {
        let (k, v) = token.split_once('=')?;
        fields.insert(k.to_string(), v.to_string());
    }
    for k in ["source", "canonical", "blob", "runner_build"] {
        if !fields.contains_key(k) {
            return None;
        }
    }
    if !["builtin", "served", "disk_cache", "canonical"].contains(&fields["source"].as_str())
        || !CANONICAL.is_match(&fields["canonical"])
        || !BLOB.is_match(&fields["blob"])
        || fields["runner_build"].is_empty()
    {
        return None;
    }
    match (fields["source"].as_str(), fields.get("canonical_sha")) {
        ("canonical", Some(sha)) if SHA12.is_match(sha) => {}
        ("canonical", _) => return None,
        (_, Some(_)) => return None,
        _ => {}
    }
    Some(fields)
}

/// `CONSISTENT` / `UNVERIFIED` for a stamped file (`classify`), through the
/// runner's own [`crate::provenance::provenance_consistent`].
fn stamp_verdict(raw: &[u8]) -> Result<(), String> {
    if stamp_fields(raw).is_none() {
        return Err(
            "the qontinui-provenance key does not parse as a stamp any runner writes".into(),
        );
    }
    let text = std::str::from_utf8(raw).map_err(|_| {
        "it is not UTF-8 text, and the runner writes every stamped file as text".to_string()
    })?;
    match crate::provenance::provenance_consistent(text) {
        Ok(_) => Ok(()),
        Err(crate::provenance::ProvenanceError::BlobMismatch { recorded, actual }) => Err(format!(
            "its body hashes to {actual}, not the blob={recorded} its stamp records: it was edited after the runner wrote it"
        )),
        Err(crate::provenance::ProvenanceError::Missing) => {
            Err("the qontinui-provenance key does not parse as a stamp any runner writes".into())
        }
    }
}

// ---------------------------------------------------------------------------
// The classifier (served_corpus_classify)
// ---------------------------------------------------------------------------

/// The judged tree and its references, with the per-pass caches.
struct Classifier<'a> {
    root: &'a Path,
    refs: &'a ServedRefs,
    git: &'a Git,
    commits: HashMap<(PathBuf, String), Result<String, String>>,
    trees: HashMap<(PathBuf, String, String), Option<HashMap<String, (String, String)>>>,
    history: HashMap<(PathBuf, String), Option<HashSet<String>>>,
    tracks_agents: Option<Option<bool>>,
}

/// A skill's `SKILL.md` stamp as an unstamped helper inherits it.
#[derive(Clone)]
enum SkillStamp {
    Fields(HashMap<String, String>),
    /// No stamp could be read. `bare` = none exists at all (no SKILL.md, or an
    /// unstamped one), the case a never-provisioned path settles.
    None {
        why: String,
        bare: bool,
    },
}

fn unreadable(repo: Option<&Path>, name: &str) -> Option<String> {
    match repo {
        None => Some(format!(
            "the workspace root is unresolved, so there is no {name} checkout"
        )),
        Some(r) if !r.is_dir() => Some(format!(
            "no {name} checkout at {}",
            quoted(&r.display().to_string())
        )),
        Some(_) => None,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

impl<'a> Classifier<'a> {
    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// The first 64 KiB of a regular (non-symlink) file, else `None`.
    fn head(&self, rel: &str) -> Option<Vec<u8>> {
        let path = self.path(rel);
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let mut buf = Vec::new();
        std::fs::File::open(&path)
            .ok()?
            .take(65536)
            .read_to_end(&mut buf)
            .ok()?;
        Some(buf)
    }

    fn whole(&self, rel: &str) -> Option<Vec<u8>> {
        let path = self.path(rel);
        if !std::fs::symlink_metadata(&path).ok()?.is_file() {
            return None;
        }
        std::fs::read(path).ok()
    }

    /// The one commit `prefix` abbreviates in `repo` (`commit_of`).
    fn commit_of(&mut self, repo: &Path, prefix: &str) -> Result<String, String> {
        let key = (repo.to_path_buf(), prefix.to_string());
        if let Some(hit) = self.commits.get(&key) {
            return hit.clone();
        }
        let answer = match self
            .git
            .stdout(repo, &["rev-parse", &format!("--disambiguate={prefix}")])
        {
            None => Err(String::new()),
            Some(out) => {
                let found: Vec<String> = String::from_utf8_lossy(&out)
                    .split_whitespace()
                    .filter(|oid| {
                        self.git.stdout(repo, &["cat-file", "-t", oid]).as_deref()
                            == Some(b"commit\n")
                    })
                    .map(str::to_string)
                    .collect();
                match found.len() {
                    1 => Ok(found[0].clone()),
                    0 => Err(format!(
                        "{prefix} is not a commit in {}",
                        quoted(&repo.display().to_string())
                    )),
                    _ => Err(format!(
                        "{prefix} is ambiguous in {}",
                        quoted(&repo.display().to_string())
                    )),
                }
            }
        };
        self.commits.insert(key, answer.clone());
        answer
    }

    /// `{path: (type, oid)}` under `base` at `rev`, or `None` (`tree`).
    fn tree(
        &mut self,
        repo: &Path,
        rev: &str,
        base: &str,
    ) -> Option<HashMap<String, (String, String)>> {
        let key = (repo.to_path_buf(), rev.to_string(), base.to_string());
        if let Some(hit) = self.trees.get(&key) {
            return hit.clone();
        }
        let entries = self
            .git
            .stdout(
                repo,
                &["ls-tree", "-r", "-z", "--full-tree", rev, "--", base],
            )
            .map(|out| {
                let mut map = HashMap::new();
                for rec in out.split(|b| *b == 0) {
                    let Some(tab) = rec.iter().position(|b| *b == b'\t') else {
                        continue;
                    };
                    let meta = String::from_utf8_lossy(&rec[..tab]);
                    let fields: Vec<&str> = meta.split_whitespace().collect();
                    if fields.len() == 3 {
                        map.insert(
                            String::from_utf8_lossy(&rec[tab + 1..]).into_owned(),
                            (fields[1].to_string(), fields[2].to_string()),
                        );
                    }
                }
                map
            });
        self.trees.insert(key, entries.clone());
        entries
    }

    /// Every path `repo`'s history (all refs, full history, no renames) has
    /// held under `base` (`ever_held`).
    fn ever_held(&mut self, repo: Option<&Path>, base: &str) -> Option<HashSet<String>> {
        let repo = repo?;
        let key = (repo.to_path_buf(), base.to_string());
        if let Some(hit) = self.history.get(&key) {
            return hit.clone();
        }
        let mut args = log_args();
        args.extend([
            "--all",
            "--full-history",
            "--format=",
            "--name-only",
            "--no-renames",
            "-z",
            "--",
            base,
        ]);
        let held = self
            .git
            .stdout(repo, &args)
            .map(|out| split_z(&out).into_iter().collect());
        self.history.insert(key, held.clone());
        held
    }

    /// Whether the judged tree's index tracks files under `.claude/agents`
    /// (`tracks_agents`); `None` when it cannot be read.
    fn tracks_agents(&mut self) -> Option<bool> {
        if let Some(hit) = self.tracks_agents {
            return hit;
        }
        let answer = self
            .git
            .stdout(self.root, &["ls-files", "-z", "--", CCFG_AGENTS])
            .map(|out| !out.is_empty());
        self.tracks_agents = Some(answer);
        answer
    }

    /// Did qontinui-claude-config `origin/main`'s history hold blob `oid` at
    /// `rel` as a regular file and then replace or delete it there
    /// (`replaced_in_ccfg`)? `None` when that history cannot be read.
    fn replaced_in_ccfg(&self, oid: &str, rel: &str) -> Option<bool> {
        let ccfg = self.refs.ccfg.as_deref()?;
        let find = format!("--find-object={oid}");
        let mut args = log_args();
        args.extend([
            "--full-history",
            "--no-renames",
            "--no-abbrev",
            "--format=",
            "--raw",
            "-z",
            &find,
            "refs/remotes/origin/main",
            "--",
            rel,
        ]);
        let out = self.git.stdout(ccfg, &args)?;
        let toks: Vec<&[u8]> = out.split(|b| *b == 0).collect();
        for i in 0..toks.len().saturating_sub(1) {
            let tok = trim_leading_newlines(toks[i]);
            let meta = String::from_utf8_lossy(tok);
            let fields: Vec<&str> = meta.split_whitespace().collect();
            if fields.len() != 5 || !fields[0].starts_with(':') {
                continue;
            }
            if toks[i + 1] != rel.as_bytes() {
                continue;
            }
            if !matches!(fields[0].strip_prefix(':'), Some("100644" | "100755")) {
                continue;
            }
            if fields[2] == oid {
                return Some(true);
            }
        }
        Some(false)
    }

    /// `{rel: oid}` by `git hash-object` in `cwd`; a failed chunk is left out.
    fn hashes(&self, cwd: &Path, rels: &[String]) -> HashMap<String, String> {
        let mut oids = HashMap::new();
        for chunk in rels.chunks(100) {
            let mut args = vec!["hash-object", "--"];
            args.extend(chunk.iter().map(String::as_str));
            if let Some(out) = self.git.stdout(cwd, &args) {
                let got: Vec<String> = String::from_utf8_lossy(&out)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                if got.len() == chunk.len() {
                    oids.extend(chunk.iter().cloned().zip(got));
                }
            }
        }
        oids
    }

    /// The definite reason for a never-held path: "ever" only when both
    /// histories are complete (`never_reason`).
    fn never_reason(&self) -> String {
        let mut notes = Vec::new();
        for (name, repo) in [
            ("qontinui-claude-config", self.refs.ccfg.as_deref()),
            ("qontinui-runner", self.refs.runner.as_deref()),
        ] {
            let out = repo.and_then(|r| {
                self.git
                    .stdout(r, &["rev-parse", "--is-shallow-repository"])
            });
            match out {
                None => notes.push(format!("whether {name} is shallow could not be read")),
                Some(o) if o.trim_ascii() == b"true" => notes.push(format!(
                    "{name} is shallow from {}",
                    self.boundary(repo.unwrap_or(self.root))
                )),
                Some(_) => {}
            }
        }
        if notes.is_empty() {
            NEVER.to_string()
        } else {
            format!(
                "no runner provisioner source held this path in the history read here ({})",
                notes.join("; ")
            )
        }
    }

    fn boundary(&self, repo: &Path) -> String {
        let shas: Vec<String> = self
            .git
            .stdout(
                repo,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "shallow",
                ],
            )
            .and_then(|p| std::fs::read_to_string(String::from_utf8_lossy(&p).trim()).ok())
            .map(|t| t.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        let Some(first) = shas.first() else {
            return "a boundary it could not read".into();
        };
        let mut args = log_args();
        args.extend(["-1", "--format=%cs", first.as_str()]);
        let day = self
            .git
            .stdout(repo, &args)
            .map(|d| String::from_utf8_lossy(&d).trim().to_string())
            .unwrap_or_default();
        let more = if shas.len() > 1 {
            format!(" and {} more", shas.len() - 1)
        } else {
            String::new()
        };
        let day = if day.is_empty() {
            String::new()
        } else {
            format!(" ({day})")
        };
        let short: String = first.chars().take(12).collect();
        format!("{short}{day}{more}")
    }

    fn skill_stamp(&mut self, cache: &mut HashMap<String, SkillStamp>, name: &str) -> SkillStamp {
        if let Some(hit) = cache.get(name) {
            return hit.clone();
        }
        let rel = format!(".claude/skills/{name}/SKILL.md");
        let path = self.path(&rel);
        let meta = std::fs::symlink_metadata(&path);
        let stamp = match meta {
            Err(_) => SkillStamp::None {
                why: format!("its skill has no {}", quoted(&rel)),
                bare: true,
            },
            Ok(m) if !m.is_file() => SkillStamp::None {
                why: format!(
                    "its skill's {} is not a readable regular file",
                    quoted(&rel)
                ),
                bare: false,
            },
            Ok(_) => match self.whole(&rel) {
                None => SkillStamp::None {
                    why: format!(
                        "its skill's {} is not a readable regular file",
                        quoted(&rel)
                    ),
                    bare: false,
                },
                Some(raw) if !has_provenance_key(&raw) => SkillStamp::None {
                    why: format!("its skill's {} carries no provenance stamp", quoted(&rel)),
                    bare: true,
                },
                Some(raw) => match (stamp_verdict(&raw), stamp_fields(&raw)) {
                    (Ok(()), Some(fields)) => SkillStamp::Fields(fields),
                    _ => SkillStamp::None {
                        why: format!("its skill's {} stamp does not verify", quoted(&rel)),
                        bare: false,
                    },
                },
            },
        };
        cache.insert(name.to_string(), stamp.clone());
        stamp
    }

    /// One verdict per candidate, in candidate order.
    fn classify(&mut self, cands: &[String]) -> BTreeMap<String, Verdict> {
        let mut verdict: BTreeMap<String, Verdict> = BTreeMap::new();
        // rel -> (repo, name, prefix, base, path in that repo)
        let mut helpers: BTreeMap<String, (Option<PathBuf>, &'static str, String, String, String)> =
            BTreeMap::new();
        let mut wanted: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut agents: Vec<String> = Vec::new();
        let mut refs: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        let mut pycs: Vec<String> = Vec::new();
        let mut skills: HashMap<String, SkillStamp> = HashMap::new();
        let mut unclaimed: Vec<String> = Vec::new();

        for rel in cands {
            let path = self.path(rel);
            let meta = std::fs::symlink_metadata(&path);
            let is_link = meta.as_ref().is_ok_and(|m| m.file_type().is_symlink());
            if is_link {
                verdict.insert(
                    rel.clone(),
                    Verdict::Uncommitted("a symlink; no runner provisioner writes one".into()),
                );
                continue;
            }
            if rel.ends_with('/') || meta.as_ref().is_ok_and(|m| m.is_dir()) {
                verdict.insert(
                    rel.clone(),
                    Verdict::Uncommitted(
                        "an embedded repository; no runner provisioner writes one".into(),
                    ),
                );
                continue;
            }
            let Some(raw) = self.head(rel) else {
                verdict.insert(
                    rel.clone(),
                    Verdict::Unknown("not a readable regular file".into()),
                );
                continue;
            };
            if has_provenance_key(&raw) {
                let fields = stamp_fields(&raw);
                let rendered = fields
                    .as_ref()
                    .and_then(|f| f["canonical"].split_once(':').map(|(_, p)| p.to_string()))
                    .unwrap_or_else(|| rel.clone());
                let v = if rendered != *rel {
                    Verdict::Uncommitted(format!(
                        "its stamp was rendered for {}, not this path",
                        quoted(&rendered)
                    ))
                } else {
                    match self.whole(rel) {
                        None => {
                            Verdict::Unknown("its provenance stamp could not be read back".into())
                        }
                        Some(full) => match stamp_verdict(&full) {
                            Ok(()) => Verdict::Residue,
                            Err(why) => Verdict::Uncommitted(why),
                        },
                    }
                };
                verdict.insert(rel.clone(), v);
            } else if PYC.is_match(rel) {
                pycs.push(rel.clone());
            } else if SKILL_FILE.is_match(rel) {
                let mut parts = rel.splitn(4, '/');
                let (_, _, name, rest) = (
                    parts.next(),
                    parts.next(),
                    parts.next().unwrap_or(""),
                    parts.next().unwrap_or(""),
                );
                match self.skill_stamp(&mut skills, name) {
                    SkillStamp::None { why, bare } => {
                        verdict.insert(rel.clone(), Verdict::Unknown(why));
                        if bare {
                            unclaimed.push(rel.clone());
                        }
                    }
                    SkillStamp::Fields(fields) => {
                        let source = fields["source"].as_str();
                        if source == "canonical" {
                            helpers.insert(
                                rel.clone(),
                                (
                                    self.refs.ccfg.clone(),
                                    "qontinui-claude-config",
                                    fields["canonical_sha"].clone(),
                                    CCFG_SKILLS.to_string(),
                                    rel.clone(),
                                ),
                            );
                        } else if source == "builtin" {
                            match BUILD.captures(&fields["runner_build"]) {
                                Some(c) => {
                                    let sha = c
                                        .get(1)
                                        .or_else(|| c.get(2))
                                        .map(|m| m.as_str().to_string())
                                        .unwrap_or_default();
                                    helpers.insert(
                                        rel.clone(),
                                        (
                                            self.refs.runner.clone(),
                                            "qontinui-runner",
                                            sha,
                                            RUNNER_SKILLS.to_string(),
                                            format!("{RUNNER_SKILLS}/{name}/{rest}"),
                                        ),
                                    );
                                }
                                None => {
                                    verdict.insert(
                                        rel.clone(),
                                        Verdict::Unknown(format!(
                                            "its skill's stamp runner_build={} names no commit",
                                            fields["runner_build"]
                                        )),
                                    );
                                }
                            }
                        } else {
                            verdict.insert(
                                rel.clone(),
                                Verdict::Unknown(format!(
                                    "its skill's SKILL.md stamp is source={source}, which no local checkout holds"
                                )),
                            );
                        }
                    }
                }
            } else if rel.starts_with(".claude/agents/") {
                agents.push(rel.clone());
            } else {
                verdict.insert(
                    rel.clone(),
                    Verdict::Uncommitted("an unstamped file no runner provisioner writes".into()),
                );
            }
        }

        for (rel, (repo, name, prefix, base, path)) in helpers {
            let resolved = match repo.as_deref() {
                Some(r) => self.commit_of(r, &prefix),
                None => Err(String::new()),
            };
            match resolved {
                Err(why) => {
                    let why = if !why.is_empty() {
                        why
                    } else {
                        unreadable(repo.as_deref(), name).unwrap_or_else(|| {
                            format!(
                                "{prefix} could not be read in {}",
                                quoted(
                                    &repo
                                        .as_deref()
                                        .map(|r| r.display().to_string())
                                        .unwrap_or_default()
                                )
                            )
                        })
                    };
                    verdict.insert(rel, Verdict::Unknown(why));
                }
                Ok(sha) => {
                    let repo = repo.expect("a commit resolved in it");
                    match self.tree(&repo, &sha, &base) {
                        None => {
                            verdict.insert(
                                rel,
                                Verdict::Unknown(format!("{name}@{prefix} could not be read")),
                            );
                        }
                        Some(entries) => match entries.get(&path) {
                            Some((kind, oid)) if kind == "blob" => {
                                wanted.insert(
                                    rel,
                                    (oid.clone(), format!("{name}@{prefix} {}", quoted(&path))),
                                );
                            }
                            _ => {
                                verdict.insert(
                                    rel.clone(),
                                    Verdict::Unknown(format!(
                                        "{name}@{prefix} holds no {}",
                                        quoted(&path)
                                    )),
                                );
                                unclaimed.push(rel);
                            }
                        },
                    }
                }
            }
        }

        if !agents.is_empty() {
            let ccfg = self.refs.ccfg.clone();
            let main_tree = ccfg
                .as_deref()
                .and_then(|c| self.tree(c, "refs/remotes/origin/main", CCFG_AGENTS))
                .unwrap_or_default();
            let copies: Vec<String> = agents
                .iter()
                .filter(|rel| {
                    ccfg.as_deref().is_some_and(|c| {
                        let theirs = c.join(rel.as_str());
                        theirs.is_file() && !same_file(&theirs, &self.path(rel))
                    })
                })
                .cloned()
                .collect();
            let theirs = match ccfg.as_deref() {
                Some(c) if !copies.is_empty() => self.hashes(c, &copies),
                _ => HashMap::new(),
            };
            for rel in &agents {
                let mut held = HashSet::new();
                if let Some(oid) = theirs.get(rel) {
                    held.insert(oid.clone());
                }
                if let Some((kind, oid)) = main_tree.get(rel) {
                    if kind == "blob" {
                        held.insert(oid.clone());
                    }
                }
                match unreadable(ccfg.as_deref(), "qontinui-claude-config") {
                    Some(why) if held.is_empty() => {
                        verdict.insert(rel.clone(), Verdict::Unknown(why));
                    }
                    _ => {
                        refs.insert(rel.clone(), held);
                    }
                }
            }
        }

        let to_hash: Vec<String> = wanted.keys().chain(refs.keys()).cloned().collect();
        let have = self.hashes(self.root, &to_hash);
        for (rel, (want, where_)) in &wanted {
            let v = match have.get(rel) {
                None => Verdict::Unknown("git hash-object could not read it".into()),
                Some(oid) if oid == want => Verdict::Residue,
                Some(_) => Verdict::Uncommitted(format!("its bytes differ from {where_}")),
            };
            verdict.insert(rel.clone(), v);
        }
        for (rel, held) in &refs {
            let Some(oid) = have.get(rel) else {
                verdict.insert(
                    rel.clone(),
                    Verdict::Unknown("git hash-object could not read it".into()),
                );
                if held.is_empty() {
                    unclaimed.push(rel.clone());
                }
                continue;
            };
            if held.contains(oid) {
                verdict.insert(rel.clone(), Verdict::Residue);
                continue;
            }
            let replaced = self.replaced_in_ccfg(oid, rel);
            let source_tree = if replaced == Some(true) {
                self.tracks_agents()
            } else {
                Some(false)
            };
            if replaced == Some(true) && source_tree == Some(false) {
                verdict.insert(rel.clone(), Verdict::Residue);
                continue;
            }
            if replaced == Some(true) {
                verdict.insert(
                    rel.clone(),
                    Verdict::Unknown(format!(
                        "it equals a version qontinui-claude-config origin/main held and replaced, but this \
                         tree {} .claude/agents itself, and where agent definitions are tracked a restored \
                         version reads exactly like an earlier spawn's copy",
                        if source_tree == Some(true) { "tracks" } else { "may track (its index could not be read)" }
                    )),
                );
                continue;
            }
            let mut why = if held.is_empty() {
                "neither qontinui-claude-config origin/main nor its checkout holds it".to_string()
            } else {
                "it equals neither qontinui-claude-config origin/main nor its checkout's copy"
                    .to_string()
            };
            if replaced.is_none() {
                why.push_str(", and whether its origin/main history held these bytes there could not be read");
            } else {
                why.push_str(", nor did the origin/main history read here hold these bytes as this file and replace them");
            }
            verdict.insert(rel.clone(), Verdict::Unknown(why));
            unclaimed.push(rel.clone());
        }

        // Item 8: a K that only says "no reference holds these bytes" becomes U
        // when neither provisioner's source has EVER held the path.
        let never = if unclaimed.is_empty() {
            String::new()
        } else {
            self.never_reason()
        };
        for rel in unclaimed {
            let (ccfg_base, runner_base, runner_path) =
                if let Some(file) = rel.strip_prefix(".claude/agents/") {
                    if let Some(c) = self.refs.ccfg.as_deref() {
                        let src = c.join(&rel);
                        if src.is_file() && !same_file(&src, &self.path(&rel)) {
                            // The overlay's source holds it: it may have written it.
                            continue;
                        }
                    }
                    (
                        CCFG_AGENTS.to_string(),
                        RUNNER_AGENTS.to_string(),
                        format!("{RUNNER_AGENTS}/{file}"),
                    )
                } else {
                    let mut parts = rel.splitn(4, '/');
                    let (_, _, name, rest) = (
                        parts.next(),
                        parts.next(),
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                    );
                    (
                        format!("{CCFG_SKILLS}/{name}"),
                        format!("{RUNNER_SKILLS}/{name}"),
                        format!("{RUNNER_SKILLS}/{name}/{rest}"),
                    )
                };
            let ccfg = self.refs.ccfg.clone();
            let runner = self.refs.runner.clone();
            let in_ccfg = self.ever_held(ccfg.as_deref(), &ccfg_base);
            let in_runner = self.ever_held(runner.as_deref(), &runner_base);
            if let (Some(a), Some(b)) = (in_ccfg, in_runner) {
                if !a.contains(&rel) && !b.contains(&runner_path) {
                    verdict.insert(rel, Verdict::Uncommitted(never.clone()));
                }
            }
        }

        // Item 5: bytecode, judged against its source's verdict.
        let mut checks: Vec<(String, &'static str, String, String, u8)> = Vec::new();
        for rel in &pycs {
            let caps = PYC.captures(rel).expect("matched above");
            let source = format!("{}{}.py", &caps[1], &caps[2]);
            let tag = caps[3].to_string();
            let opt: u8 = caps.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0));
            if !pyc_opens(&self.path(rel)) {
                verdict.insert(
                    rel.clone(),
                    Verdict::Uncommitted(format!(
                        "named as interpreter bytecode of {}, but it does not open as any interpreter's \
                         bytecode does (a magic number ending in CR LF, in at least 8 bytes)",
                        quoted(&source)
                    )),
                );
                continue;
            }
            match verdict.get(&source) {
                Some(Verdict::Residue) => checks.push((rel.clone(), "mismatch", source, tag, opt)),
                Some(_) => {
                    verdict.insert(
                        rel.clone(),
                        Verdict::Unknown(format!(
                            "compiled from {}, which is not shown to be the runner's",
                            quoted(&source)
                        )),
                    );
                }
                None if self.path(&source).is_file() => {
                    checks.push((rel.clone(), "not_cache", source, tag, opt))
                }
                None => {
                    verdict.insert(
                        rel.clone(),
                        Verdict::Unknown(format!(
                            "compiled from {}, which is not in this tree",
                            quoted(&source)
                        )),
                    );
                }
            }
        }
        if !checks.is_empty() {
            let reasons = run_pyc_checks(self.root, &checks, self.git.per_call);
            for (i, (rel, mode, source, _, _)) in checks.iter().enumerate() {
                let v = match reasons.as_ref().map(|r| r.get(i).cloned()) {
                    Err(why) => Verdict::Unknown(format!(
                        "compiled from {}, and its bytecode could not be checked: {why}",
                        quoted(source)
                    )),
                    Ok(None) => Verdict::Unknown(format!(
                        "compiled from {}, and the bytecode check gave no answer for it",
                        quoted(source)
                    )),
                    Ok(Some(why)) if *mode == "mismatch" => {
                        if why.is_empty() {
                            Verdict::Residue
                        } else {
                            Verdict::Unknown(why)
                        }
                    }
                    Ok(Some(why)) => {
                        if why.is_empty() {
                            Verdict::LeftOut
                        } else {
                            Verdict::Unknown(format!(
                                "compiled from {}, which is tracked here, but {why}",
                                quoted(source)
                            ))
                        }
                    }
                };
                verdict.insert(rel.clone(), v);
            }
        }
        verdict
    }
}

fn trim_leading_newlines(mut tok: &[u8]) -> &[u8] {
    while let [b'\n', rest @ ..] = tok {
        tok = rest;
    }
    tok
}

/// Every parsed `git log` pins the config that reshapes its output.
fn log_args() -> Vec<&'static str> {
    vec![
        "-c",
        "log.showSignature=false",
        "-c",
        "log.showRoot=true",
        "-c",
        "log.follow=false",
        "-c",
        "diff.relative=false",
        "log",
        "--no-show-signature",
        "--no-color",
        "--no-decorate",
        "--no-ext-diff",
    ]
}

/// The file opens as every interpreter's bytecode does: at least 8 bytes, a
/// magic number ending in CR LF (`pyc_magic`).
fn pyc_opens(path: &Path) -> bool {
    let mut lead = [0u8; 8];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut lead))
        .is_ok()
        && &lead[2..4] == b"\r\n"
}

/// Run `served_residue_pyc.py` over every bytecode check, under `python3 -I`
/// (then `python`). One reason per item, `''` = passed; `Err` = not run.
fn run_pyc_checks(
    root: &Path,
    checks: &[(String, &'static str, String, String, u8)],
    bound: Duration,
) -> Result<Vec<String>, String> {
    let items: Vec<serde_json::Value> = checks
        .iter()
        .map(|(rel, mode, source, tag, opt)| {
            serde_json::json!({"mode": mode, "rel": rel, "source": source, "tag": tag, "opt": opt})
        })
        .collect();
    let request =
        serde_json::json!({"root": root.display().to_string(), "items": items}).to_string();
    let mut last = String::from("no python3/python on PATH");
    for interpreter in ["python3", "python"] {
        let out = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        let spawned = crate::process_helpers::no_window(interpreter)
            .arg("-I")
            .arg("-c")
            .arg(PYC_CHECK)
            .arg(&request)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out.reopen().map_err(|e| e.to_string())?))
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                last = format!("{interpreter} could not run: {e}");
                continue;
            }
        };
        let until = Instant::now() + bound;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() >= until => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{interpreter} did not finish within {} s",
                        bound.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(e.to_string()),
            }
        };
        if !status.success() {
            return Err(format!("{interpreter} exited {status}"));
        }
        let text = std::fs::read(out.path()).map_err(|e| e.to_string())?;
        let reasons: Vec<String> = serde_json::from_slice(&text)
            .map_err(|e| format!("{interpreter} printed no verdict list: {e}"))?;
        if reasons.len() != checks.len() {
            return Err(format!(
                "{interpreter} gave {} answer(s) for {} file(s)",
                reasons.len(),
                checks.len()
            ));
        }
        return Ok(reasons);
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// judge_changes
// ---------------------------------------------------------------------------

/// Is a porcelain entry one the served-corpus listing judges itself?
fn porcelain_is_served(line: &str) -> bool {
    let Some(path) = line.strip_prefix("?? ") else {
        return false;
    };
    let path = path.strip_prefix('"').unwrap_or(path);
    SERVED_ROOTS
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

fn listed_path_is_served(path: &str) -> bool {
    SERVED_ROOTS
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

fn more(n: usize) -> String {
    if n > SERVED_LIST_MAX {
        format!(" (+{} more)", n - SERVED_LIST_MAX)
    } else {
        String::new()
    }
}

/// Judge the `uncommitted` arm of one tree from its `git status --porcelain
/// --untracked-files=normal` output — `judge_changes`. `status_warning` is
/// `git status`'s stderr gist when it wrote any (item 12).
///
/// Every porcelain entry is a definite reason except the bare collapsed
/// `?? .claude/` and an untracked path under a served root. Inside a collapsed
/// `?? .claude/` a non-ignored, non-served file is definite too, and every
/// untracked file under a served root — ignored or excluded too — goes through
/// the residue rule. A definite reason outranks every unknown.
pub(crate) fn judge_changes(
    root: &Path,
    status: &str,
    status_warning: Option<&str>,
    refs: &ServedRefs,
    git: &Git,
) -> ChangesJudgement {
    let mut unknowns: Vec<String> = Vec::new();
    if let Some(warning) = status_warning {
        unknowns.push(format!(
            "git status warned ({warning}); what it could not open is left out of its listing, so it cannot be judged"
        ));
    }
    let entries: Vec<&str> = status.lines().filter(|l| !l.is_empty()).collect();
    let mut collapsed = false;
    let mut n_out = 0usize;
    for line in &entries {
        if *line == "?? .claude/" {
            collapsed = true;
        } else if !porcelain_is_served(line) {
            n_out += 1;
        }
    }

    let (mut n_r, mut n_x) = (0usize, 0usize);
    let mut u_list: Vec<String> = Vec::new();
    let mut k_list: Vec<String> = Vec::new();
    let mut unjudged: Option<String> = None;
    let claude = root.join(".claude");
    let claude_is_dir = std::fs::symlink_metadata(&claude).is_ok_and(|m| m.is_dir());
    if claude_is_dir {
        let others = git.run(
            root,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                ".claude",
            ],
        );
        let others = match others {
            Some(o) if o.ok() && o.stderr.is_empty() => Some(split_z(&o.stdout)),
            Some(o) => {
                let why = if o.ok() {
                    format!("it warned: {}", stderr_gist(&o.stderr))
                } else {
                    format!("it exited {:?}: {}", o.code, stderr_gist(&o.stderr))
                };
                unknowns.push(format!(
                    "git ls-files --others --exclude-standard -- .claude failed or warned ({why}), so what else .claude/ holds cannot be judged"
                ));
                None
            }
            None => {
                unknowns.push("git ls-files --others --exclude-standard -- .claude did not answer, so what else .claude/ holds cannot be judged".into());
                None
            }
        };
        let mut args = vec!["ls-files", "--others", "-z", "--"];
        args.extend(SERVED_ROOTS);
        let cands = match git.run(root, &args) {
            Some(o) if o.ok() && o.stderr.is_empty() => Some(split_z(&o.stdout)),
            Some(o) => {
                let why = if o.ok() {
                    format!("it warned: {}", stderr_gist(&o.stderr))
                } else {
                    format!("it exited {:?}: {}", o.code, stderr_gist(&o.stderr))
                };
                unknowns.push(format!(
                    "git ls-files --others -- {SERVED_LABEL} failed or warned ({why}), so its served-corpus files cannot be judged"
                ));
                None
            }
            None => {
                unknowns.push(format!("git ls-files --others -- {SERVED_LABEL} did not answer, so its served-corpus files cannot be judged"));
                None
            }
        };
        if collapsed {
            for path in others.iter().flatten() {
                if !listed_path_is_served(path) {
                    n_out += 1;
                }
            }
        }
        if let Some(cands) = cands.filter(|c| !c.is_empty()) {
            let mut classifier = Classifier {
                root,
                refs,
                git,
                commits: HashMap::new(),
                trees: HashMap::new(),
                history: HashMap::new(),
                tracks_agents: None,
            };
            let verdicts = classifier.classify(&cands);
            if verdicts.len() != cands.len() || Instant::now() >= git.deadline {
                unjudged = Some(format!(
                    "{} untracked file(s) under {SERVED_LABEL} were not judged: the classifier {}",
                    cands.len(),
                    if Instant::now() >= git.deadline {
                        "ran out of time"
                    } else {
                        "did not give one verdict per file"
                    }
                ));
            }
            for rel in &cands {
                match verdicts.get(rel) {
                    Some(Verdict::Residue) => n_r += 1,
                    Some(Verdict::LeftOut) => n_x += 1,
                    Some(Verdict::Uncommitted(why)) => {
                        u_list.push(format!("{}: {why}", quoted(rel)))
                    }
                    Some(Verdict::Unknown(why)) => k_list.push(format!("{}: {why}", quoted(rel))),
                    None => {}
                }
            }
        }
    }

    let n_u = u_list.len();
    let n_k = k_list.len();
    let mut judgement = ChangesJudgement {
        residue: n_r,
        ..Default::default()
    };
    if n_out + n_u > 0 {
        let tail = if n_u == 0 {
            String::new()
        } else {
            let shown: Vec<&str> = u_list
                .iter()
                .take(SERVED_LIST_MAX)
                .map(String::as_str)
                .collect();
            format!(
                "; {n_u} under {SERVED_LABEL} are not the runner's own bytes: {}{}",
                shown.join("; "),
                more(n_u)
            )
        };
        judgement.uncommitted = Some(format!(
            "{} has uncommitted changes or untracked files ({} path(s)){tail}",
            root.display(),
            n_out + n_u
        ));
        return judgement;
    }
    if let Some(why) = unjudged {
        unknowns.push(why);
    } else if n_k > 0 {
        let shown: Vec<&str> = k_list
            .iter()
            .take(SERVED_LIST_MAX)
            .map(String::as_str)
            .collect();
        unknowns.push(format!(
            "{n_k} untracked file(s) under {SERVED_LABEL} cannot be shown to be the runner's own bytes: {}{}",
            shown.join("; "),
            more(n_k)
        ));
    }
    if unknowns.is_empty() && n_r == 0 && n_x == 0 && !entries.is_empty() {
        // git status listed something, and judging it gave no reason, no
        // verdict and no left-out cache: what it holds went unjudged.
        unknowns.push(format!(
            "git status listed {} entr(ies), and judging them gave no reason, no residue verdict and no left-out cache, so what they hold went unjudged",
            entries.len()
        ));
    }
    if !unknowns.is_empty() {
        judgement.unknown = Some(format!("{}: {}", root.display(), unknowns.join("; ")));
    }
    judgement
}

#[cfg(test)]
mod tests;
