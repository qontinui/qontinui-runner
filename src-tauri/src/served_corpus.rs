//! Which `.claude/` tree a spawned session is served, how stale it is, and
//! whether its files are THIS build's bundle.
//!
//! Plan `2026-09-03-served-corpus-provenance-at-spawn`. A session is served a
//! slash-command body, a skill or a hook out of `<workdir>/.claude/`, and on
//! this fleet that directory is often a symlink into a shared, tracked
//! `qontinui-claude-config` checkout. That checkout drifts from `origin/main`,
//! and a runner build that predates the tracked-destination guard can overwrite
//! it with the binary's embedded bundle. Neither leaves a mark a session can
//! read without a `git` call of its own, and the harness expands a command body
//! before the session's first turn, so no reader-side rule can reach it.
//!
//! The runner is the one party that can answer at spawn, for free: it holds the
//! bundle bytes in memory. This module measures the served tree once per spawn
//! and renders ONE key-addressed header line of `QONTINUI_RUNNER_CONTEXT`:
//!
//! ```text
//! [served-corpus: <canonical .claude>] [checkout: <repo> <branch>@<sha12> upstream=<ref> behind=<n> ahead=<m> as-of=<fetch-ts> dirty-claude=<k>] [bundle: <N>/<M> identical-to-build <gitSha> stamped=<k>] [provisioned: …] [cwd: …]
//! ```
//!
//! Every value is a measurement or `UNKNOWN (<reason>)`, never a default. The
//! tokens are cut at their own first `]`, the line-2 grammar, so no rendered
//! value ever carries a `]`.
//!
//! ## Where the I/O happens
//!
//! [`crate::terminal::runner_context`] is a zero-I/O renderer by contract. The
//! SEAM that spawns a session calls [`probe`] and hands the result in, the same
//! way it hands in the per-session `CoordMcpDelivery`. [`probe`] is bounded:
//! each `git` spawn runs under [`PROBE_TIMEOUT`] through
//! [`crate::process_helpers::run_with_timeout_detailed`], which kills the whole
//! process tree on expiry. The first timeout or missing binary short-circuits
//! every later `git` call in the same probe, so a hung `git` costs one timeout,
//! not one per question. It never fetches, never writes, and runs `git status`
//! with `GIT_OPTIONAL_LOCKS=0` so not even the index is refreshed.
//!
//! ## Why the answer is memoised
//!
//! A seam renders the briefing twice: an argv copy BEFORE it provisions the
//! session and an env copy AFTER. Two independent probes could disagree across
//! that write. The memo keys on `(canonical workdir, RUNNER_BUILD_ID)` for
//! [`MEMO_TTL`], so both copies carry the same measurement.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Wall-clock bound on each `git` spawn a probe runs. The same class as
/// `provision_guard::PROBE_TIMEOUT`: generous for a local read, tight for a
/// spawn an operator is waiting on.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a probe result is reused for the same workdir and build.
const MEMO_TTL: Duration = Duration::from_secs(30);

/// The commit this binary was built from, as `/health` reports `gitSha`.
const BUILD_SHA: &str = env!("QONTINUI_GIT_SHA");

/// The build identity the memo keys on, as `/health` reports `buildId`.
const RUNNER_BUILD: &str = env!("RUNNER_BUILD_ID");

/// Upstream ref used when `refs/remotes/origin/HEAD` names none.
const DEFAULT_UPSTREAM: &str = "refs/remotes/origin/main";

/// A measured value, or the reason it could not be measured.
///
/// The reason is DATA that ends up on the header line, not a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Probe<T> {
    Measured(T),
    Unknown(String),
}

/// The branch and commit a checkout's `HEAD` points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    /// `None` on a detached `HEAD`.
    branch: Option<String>,
    /// Full 40-hex commit id.
    sha: String,
}

/// How far `HEAD` is from its upstream, read from the LOCAL remote-tracking ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Upstream {
    /// Short name, e.g. `origin/main`.
    name: String,
    behind: u64,
    ahead: u64,
}

/// One git work tree, as the probe measured it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoState {
    /// Canonical `git rev-parse --show-toplevel`.
    toplevel: PathBuf,
    head: Probe<Head>,
    upstream: Probe<Upstream>,
    /// When the upstream ref was last fetched, RFC 3339 UTC.
    as_of: Probe<String>,
    /// Tracked entries `git status` reports as changed, in the probed scope.
    dirty: Probe<usize>,
}

/// Whether a path is inside a git work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checkout {
    /// A stated non-checkout, not an UNKNOWN.
    NotAWorkTree,
    Repo(RepoState),
}

/// The `cwd` token: the workdir's own checkout, unless it IS the served one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdCheckout {
    SameAsCorpus,
    Other(Checkout),
}

/// How many of the binary's bundled files the served tree holds unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BundleIdentity {
    /// Files identical to this build's copy, after EOL normalisation and after
    /// removing a `qontinui-provenance:` key.
    identical: usize,
    /// Every file this binary carries: commands plus every skill file.
    total: usize,
    /// Files that carried a `qontinui-provenance:` key. Canonical sources are
    /// never stamped, so a stamped file inside a tracked tree is a clobber
    /// proven by the file itself.
    stamped: usize,
}

/// What a spawn was served, measured once at the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServedCorpus {
    corpus: Probe<PathBuf>,
    checkout: Probe<Checkout>,
    bundle: Probe<BundleIdentity>,
    provisioned: Probe<String>,
    cwd: Probe<CwdCheckout>,
}

impl ServedCorpus {
    /// The value for a call site with no session to measure. Every token reads
    /// `UNKNOWN (<reason>)`, so a caller never fabricates a measurement.
    pub(crate) fn unknown(reason: &'static str) -> ServedCorpus {
        let r = || reason.to_string();
        ServedCorpus {
            corpus: Probe::Unknown(r()),
            checkout: Probe::Unknown(r()),
            bundle: Probe::Unknown(r()),
            provisioned: Probe::Unknown(r()),
            cwd: Probe::Unknown(r()),
        }
    }

    /// The header line, without a trailing newline.
    ///
    /// When the whole value is UNKNOWN for one reason, a single token says so:
    /// five tokens repeating "no session" would be noise.
    pub(crate) fn render_line(&self) -> String {
        if let Probe::Unknown(reason) = &self.corpus {
            let all_same = [
                unknown_reason(&self.checkout),
                unknown_reason(&self.bundle),
                unknown_reason(&self.provisioned),
                unknown_reason(&self.cwd),
            ]
            .iter()
            .all(|r| *r == Some(reason.as_str()));
            if all_same {
                return format!("[served-corpus: UNKNOWN ({})]", clean(reason));
            }
        }
        let corpus = match &self.corpus {
            Probe::Measured(p) => clean(&p.display().to_string()),
            Probe::Unknown(r) => unknown_text(r),
        };
        let checkout = match &self.checkout {
            Probe::Measured(c) => render_checkout(c, "dirty-claude", true),
            Probe::Unknown(r) => unknown_text(r),
        };
        let bundle = match &self.bundle {
            Probe::Measured(b) => format!(
                "{}/{} identical-to-build {BUILD_SHA} stamped={}",
                b.identical, b.total, b.stamped
            ),
            Probe::Unknown(r) => unknown_text(r),
        };
        let provisioned = match &self.provisioned {
            Probe::Measured(p) => clean(p),
            Probe::Unknown(r) => unknown_text(r),
        };
        let cwd = match &self.cwd {
            Probe::Measured(CwdCheckout::SameAsCorpus) => "same checkout".to_string(),
            Probe::Measured(CwdCheckout::Other(c)) => render_checkout(c, "dirty", false),
            Probe::Unknown(r) => unknown_text(r),
        };
        format!(
            "[served-corpus: {corpus}] [checkout: {checkout}] [bundle: {bundle}] \
             [provisioned: {provisioned}] [cwd: {cwd}]"
        )
    }
}

fn unknown_reason<T>(p: &Probe<T>) -> Option<&str> {
    match p {
        Probe::Unknown(r) => Some(r.as_str()),
        Probe::Measured(_) => None,
    }
}

/// `UNKNOWN (<reason>)`, with the reason made safe for the token grammar.
fn unknown_text(reason: &str) -> String {
    format!("UNKNOWN ({})", clean(reason))
}

/// A value safe inside one `[key: value]` token: no `]` (it would end the
/// token early) and no line break (it would end the header line).
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ']' => ')',
            '[' => '(',
            '\n' | '\r' => ' ',
            c => c,
        })
        .collect()
}

fn render_checkout(c: &Checkout, dirty_key: &str, with_ahead: bool) -> String {
    let repo = match c {
        Checkout::NotAWorkTree => return "none (not a git work tree)".to_string(),
        Checkout::Repo(r) => r,
    };
    let name = repo
        .toplevel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.toplevel.display().to_string());
    let head = match &repo.head {
        Probe::Measured(h) => format!(
            "{}@{}",
            h.branch.as_deref().unwrap_or("(detached)"),
            h.sha.get(..12).unwrap_or(&h.sha)
        ),
        Probe::Unknown(r) => format!("HEAD={}", unknown_text(r)),
    };
    let upstream = match &repo.upstream {
        Probe::Measured(u) if with_ahead => {
            format!("upstream={} behind={} ahead={}", u.name, u.behind, u.ahead)
        }
        Probe::Measured(u) => format!("behind={}", u.behind),
        Probe::Unknown(r) if with_ahead => format!("upstream={}", unknown_text(r)),
        Probe::Unknown(r) => format!("behind={}", unknown_text(r)),
    };
    let as_of = match &repo.as_of {
        Probe::Measured(ts) => ts.clone(),
        Probe::Unknown(r) => unknown_text(r),
    };
    let dirty = match &repo.dirty {
        Probe::Measured(n) => n.to_string(),
        Probe::Unknown(r) => unknown_text(r),
    };
    format!(
        "{} {head} {upstream} as-of={as_of} {dirty_key}={dirty}",
        clean(&name)
    )
}

// ── The probe ───────────────────────────────────────────────────────────────

type MemoKey = (PathBuf, &'static str);

fn memo() -> &'static Mutex<HashMap<MemoKey, (Instant, ServedCorpus)>> {
    static MEMO: OnceLock<Mutex<HashMap<MemoKey, (Instant, ServedCorpus)>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Measure what `<workdir>/.claude/` serves, reusing a measurement of the same
/// workdir by the same build taken within [`MEMO_TTL`].
///
/// Never fails and never panics: it runs on a spawn path, and every failure is
/// an `UNKNOWN (<reason>)` token.
pub(crate) fn probe(workdir: &Path) -> ServedCorpus {
    let key_dir = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let key = (key_dir, RUNNER_BUILD);
    let now = Instant::now();
    {
        let guard = memo().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, value)) = guard.get(&key) {
            if now.duration_since(*at) < MEMO_TTL {
                return value.clone();
            }
        }
    }
    let value = probe_with(workdir, OsStr::new("git"), PROBE_TIMEOUT);
    let mut guard = memo().lock().unwrap_or_else(|p| p.into_inner());
    guard.retain(|_, (at, _)| now.duration_since(*at) < MEMO_TTL);
    guard.insert(key, (now, value.clone()));
    value
}

/// [`probe`] without the memo, with the `git` program and the per-spawn
/// timeout as parameters so a test can point them at nothing or at a hang.
fn probe_with(workdir: &Path, git_program: &OsStr, timeout: Duration) -> ServedCorpus {
    let git = Git::new(git_program, timeout);
    let claude = workdir.join(".claude");
    let corpus = match std::fs::canonicalize(&claude) {
        Ok(p) if p.is_dir() => Probe::Measured(p),
        Ok(_) => Probe::Unknown("not a directory".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if claude.symlink_metadata().is_ok() {
                Probe::Unknown("dangling symlink".to_string())
            } else {
                Probe::Unknown("absent".to_string())
            }
        }
        Err(e) => Probe::Unknown(format!("unreadable: {e}")),
    };

    let bundle = Probe::Measured(bundle_identity(match &corpus {
        Probe::Measured(p) => Some(p.as_path()),
        Probe::Unknown(_) => None,
    }));

    let checkout = match &corpus {
        Probe::Measured(dir) => probe_checkout(&git, dir, DirtyScope::Subtree),
        Probe::Unknown(_) => Probe::Unknown("no served corpus".to_string()),
    };

    let cwd = probe_cwd(&git, workdir, &checkout);

    ServedCorpus {
        corpus,
        checkout,
        bundle,
        provisioned: Probe::Unknown("ledger read lands in Phase 5".to_string()),
        cwd,
    }
}

/// The `cwd` token: `same checkout` when the workdir's work tree is the served
/// corpus's, else the workdir measured on its own.
fn probe_cwd(
    git: &Git<'_>,
    workdir: &Path,
    corpus_checkout: &Probe<Checkout>,
) -> Probe<CwdCheckout> {
    let located = match locate(git, workdir) {
        Ok(l) => l,
        Err(reason) => return Probe::Unknown(reason),
    };
    let Some(located) = located else {
        return match corpus_checkout {
            Probe::Measured(Checkout::NotAWorkTree) => Probe::Measured(CwdCheckout::SameAsCorpus),
            _ => Probe::Measured(CwdCheckout::Other(Checkout::NotAWorkTree)),
        };
    };
    if let Probe::Measured(Checkout::Repo(r)) = corpus_checkout {
        if r.toplevel == located.toplevel {
            return Probe::Measured(CwdCheckout::SameAsCorpus);
        }
    }
    Probe::Measured(CwdCheckout::Other(Checkout::Repo(measure(
        git,
        workdir,
        located,
        DirtyScope::WholeTree,
    ))))
}

/// Which paths `dirty` counts.
#[derive(Clone, Copy)]
enum DirtyScope {
    /// Only under the probed directory — the served `.claude/`.
    Subtree,
    /// The whole work tree.
    WholeTree,
}

fn probe_checkout(git: &Git<'_>, dir: &Path, scope: DirtyScope) -> Probe<Checkout> {
    match locate(git, dir) {
        Ok(None) => Probe::Measured(Checkout::NotAWorkTree),
        Ok(Some(located)) => Probe::Measured(Checkout::Repo(measure(git, dir, located, scope))),
        Err(reason) => Probe::Unknown(reason),
    }
}

/// What one `git rev-parse` says about the work tree around a directory.
struct Located {
    toplevel: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    head: Probe<Head>,
}

/// `Ok(None)` when `dir` is not inside a git work tree.
fn locate(git: &Git<'_>, dir: &Path) -> Result<Option<Located>, String> {
    let out = git.run(
        dir,
        &[
            "rev-parse",
            "--show-toplevel",
            "--absolute-git-dir",
            "--git-common-dir",
            "HEAD",
            "--abbrev-ref",
            "HEAD",
        ],
    )?;
    if !out.success && out.stderr.contains("not a git repository") {
        return Ok(None);
    }
    let lines: Vec<&str> = out.stdout.lines().collect();
    let (Some(top), Some(gd), Some(cd)) = (lines.first(), lines.get(1), lines.get(2)) else {
        return Err(format!("git rev-parse failed: {}", first_line(&out.stderr)));
    };
    let toplevel = std::fs::canonicalize(top).unwrap_or_else(|_| PathBuf::from(top));
    let git_dir = PathBuf::from(gd);
    // `--git-common-dir` is relative to the directory git ran in.
    let common_dir = {
        let p = PathBuf::from(cd);
        if p.is_absolute() {
            p
        } else {
            dir.join(p)
        }
    };
    let head = if out.success {
        match (lines.get(3), lines.get(4)) {
            (Some(sha), Some(branch)) => Probe::Measured(Head {
                sha: (*sha).to_string(),
                branch: (*branch != "HEAD").then(|| (*branch).to_string()),
            }),
            _ => Probe::Unknown("git rev-parse printed no HEAD".to_string()),
        }
    } else if out.stderr.contains("unknown revision") {
        Probe::Unknown("no commit on HEAD".to_string())
    } else {
        Probe::Unknown(format!("git rev-parse failed: {}", first_line(&out.stderr)))
    };
    Ok(Some(Located {
        toplevel,
        git_dir,
        common_dir,
        head,
    }))
}

fn measure(git: &Git<'_>, dir: &Path, located: Located, scope: DirtyScope) -> RepoState {
    let upstream_ref = upstream_ref(&located.common_dir);
    let upstream_short = upstream_ref
        .strip_prefix("refs/remotes/")
        .unwrap_or(&upstream_ref)
        .to_string();

    let upstream = match &located.head {
        Probe::Unknown(r) => Probe::Unknown(r.clone()),
        Probe::Measured(_) => {
            let range = format!("{upstream_ref}...HEAD");
            match git.run(dir, &["rev-list", "--count", "--left-right", &range]) {
                Err(reason) => Probe::Unknown(reason),
                Ok(out) if !out.success => {
                    if out.stderr.contains("unknown revision")
                        || out.stderr.contains("bad revision")
                    {
                        Probe::Unknown(format!("no {upstream_short} ref"))
                    } else {
                        Probe::Unknown(format!("git rev-list failed: {}", first_line(&out.stderr)))
                    }
                }
                Ok(out) => {
                    let mut counts = out.stdout.split_whitespace().map(str::parse::<u64>);
                    match (counts.next(), counts.next()) {
                        (Some(Ok(behind)), Some(Ok(ahead))) => Probe::Measured(Upstream {
                            name: upstream_short.clone(),
                            behind,
                            ahead,
                        }),
                        _ => Probe::Unknown("git rev-list printed no counts".to_string()),
                    }
                }
            }
        }
    };

    let as_of = fetch_age(&located, &upstream_ref);

    let status_args: &[&str] = match scope {
        DirtyScope::Subtree => &["status", "--porcelain", "--untracked-files=no", "--", "."],
        DirtyScope::WholeTree => &["status", "--porcelain", "--untracked-files=no"],
    };
    let dirty = match git.run(dir, status_args) {
        Err(reason) => Probe::Unknown(reason),
        Ok(out) if !out.success => {
            Probe::Unknown(format!("git status failed: {}", first_line(&out.stderr)))
        }
        Ok(out) => Probe::Measured(out.stdout.lines().filter(|l| !l.is_empty()).count()),
    };

    RepoState {
        toplevel: located.toplevel,
        head: located.head,
        upstream,
        as_of,
        dirty,
    }
}

/// The upstream ref to measure against: what `refs/remotes/origin/HEAD` names,
/// else `refs/remotes/origin/main`. A file read, not a spawn — `origin/HEAD`
/// is always a loose symbolic ref.
fn upstream_ref(common_dir: &Path) -> String {
    std::fs::read_to_string(common_dir.join("refs/remotes/origin/HEAD"))
        .ok()
        .and_then(|s| s.trim().strip_prefix("ref: ").map(str::to_string))
        .filter(|r| r.starts_with("refs/remotes/"))
        .unwrap_or_else(|| DEFAULT_UPSTREAM.to_string())
}

/// When the upstream ref was last fetched: the newest of `FETCH_HEAD`'s mtime
/// (per-worktree and common) and the newest reflog entry of the upstream ref.
/// A `stat` and a bounded file read — never a spawn, never a fetch.
fn fetch_age(located: &Located, upstream_ref: &str) -> Probe<String> {
    let mut newest: Option<SystemTime> = None;
    let mut consider = |t: SystemTime| {
        if newest.is_none_or(|n| t > n) {
            newest = Some(t);
        }
    };
    for dir in [&located.git_dir, &located.common_dir] {
        if let Ok(t) = std::fs::metadata(dir.join("FETCH_HEAD")).and_then(|m| m.modified()) {
            consider(t);
        }
    }
    if let Some(t) = newest_reflog_time(&located.common_dir.join("logs").join(upstream_ref)) {
        consider(t);
    }
    match newest {
        Some(t) => Probe::Measured(
            chrono::DateTime::<chrono::Utc>::from(t)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        None => Probe::Unknown("never fetched".to_string()),
    }
}

/// The timestamp of the last entry of a reflog file, reading at most its last
/// 8 KiB. An entry is `<old> <new> <ident> <unix-ts> <tz>\t<message>`.
fn newest_reflog_time(path: &Path) -> Option<SystemTime> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 8 * 1024;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    let (head, _msg) = line.split_once('\t').unwrap_or((line, ""));
    let mut fields = head.split_whitespace().rev();
    let _tz = fields.next()?;
    let secs: u64 = fields.next()?.parse().ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

// ── Bundle identity ─────────────────────────────────────────────────────────

/// Every file this binary carries, as `(path relative to .claude/, bytes)`:
/// each `FLEET_COMMANDS` entry as `commands/<name>.md`, each embedded skill
/// file as `skills/<name>/<rel>`. Counted from the registries, never
/// hard-coded.
fn bundled_files() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = crate::fleet_commands::FLEET_COMMANDS
        .iter()
        .map(|(name, body)| (format!("commands/{name}.md"), (*body).to_string()))
        .collect();
    for skill in crate::fleet_skills::embedded_skills() {
        for (rel, body) in skill.files {
            out.push((format!("skills/{}/{rel}", skill.name), body));
        }
    }
    out
}

/// CRLF → LF, so a checkout with `core.autocrlf` does not read as a fork.
fn normalize_eol(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Compare the served tree against the bundle. `None` (no served tree) counts
/// every file as missing, which is a measurement: `0/M`.
fn bundle_identity(corpus: Option<&Path>) -> BundleIdentity {
    let files = bundled_files();
    let total = files.len();
    let mut identical = 0;
    let mut stamped = 0;
    if let Some(corpus) = corpus {
        for (rel, embedded) in &files {
            let Ok(on_disk) = std::fs::read_to_string(corpus.join(rel)) else {
                continue;
            };
            let body = match crate::fleet_commands::strip_provenance(&on_disk) {
                Some((_, body)) => {
                    stamped += 1;
                    body
                }
                None => on_disk,
            };
            if normalize_eol(&body) == normalize_eol(embedded) {
                identical += 1;
            }
        }
    }
    BundleIdentity {
        identical,
        total,
        stamped,
    }
}

// ── Bounded git ─────────────────────────────────────────────────────────────

/// The captured result of one completed `git` run.
struct GitOut {
    success: bool,
    stdout: String,
    stderr: String,
}

/// A `git` runner for ONE probe. The first spawn failure or timeout is sticky:
/// every later call returns the same reason at once, so a hung or missing `git`
/// costs the probe one timeout rather than one per question.
struct Git<'a> {
    program: &'a OsStr,
    timeout: Duration,
    dead: std::cell::RefCell<Option<String>>,
}

impl<'a> Git<'a> {
    fn new(program: &'a OsStr, timeout: Duration) -> Self {
        Git {
            program,
            timeout,
            dead: std::cell::RefCell::new(None),
        }
    }

    /// `git -C <dir> --literal-pathspecs <args…>`, bounded, read-only.
    ///
    /// `Err` is a reason fit for an `UNKNOWN (…)` token.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<GitOut, String> {
        if let Some(reason) = self.dead.borrow().as_ref() {
            return Err(reason.clone());
        }
        let mut cmd = crate::process_helpers::no_window(self.program);
        cmd.arg("-C")
            .arg(dir)
            .arg("--literal-pathspecs")
            .args(args)
            // An inherited GIT_DIR / GIT_WORK_TREE / GIT_INDEX_FILE would make
            // `-C` consult the WRONG repository — the same discipline as
            // `provision_guard`'s probe.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            // `git status` otherwise refreshes and rewrites the index: this
            // probe never writes.
            .env("GIT_OPTIONAL_LOCKS", "0");
        use crate::process_helpers::TimedOutput;
        match crate::process_helpers::run_with_timeout_detailed(cmd, self.timeout) {
            Err(e) => {
                let reason = if e.kind() == std::io::ErrorKind::NotFound {
                    "git unavailable".to_string()
                } else {
                    format!("git unavailable: {e}")
                };
                *self.dead.borrow_mut() = Some(reason.clone());
                Err(reason)
            }
            Ok(run) => match run.outcome {
                TimedOutput::TimedOut { .. } => {
                    let reason = format!("git timed out after {:?}", self.timeout);
                    *self.dead.borrow_mut() = Some(reason.clone());
                    Err(reason)
                }
                TimedOutput::Completed(_) if run.truncation.is_some() => {
                    Err("git output incomplete".to_string())
                }
                TimedOutput::Completed(out) => Ok(GitOut {
                    success: out.status.success(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_commands::{AgentCommandRegistry, CommandSource};
    use crate::agent_skills::AgentSkillRegistry;
    use crate::provision_guard::test_support::assert_not_in_any_repo;
    use std::process::{Command, Stdio};

    /// Run git in `dir` with no dependence on the user's global hooks or
    /// signing config.
    fn git(dir: &Path, args: &[&str]) -> String {
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

    fn total() -> usize {
        bundled_files().len()
    }

    /// A committed checkout whose `.claude/` holds every bundled file verbatim.
    fn checkout_with_bundle() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        git(tmp.path(), &["init", "--quiet"]);
        for (rel, body) in bundled_files() {
            let p = tmp.path().join(".claude").join(&rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
        }
        git(tmp.path(), &["add", "--", ".claude"]);
        git(
            tmp.path(),
            &["commit", "--quiet", "--no-verify", "-m", "bundle"],
        );
        tmp
    }

    fn line_for(dir: &Path) -> String {
        probe_with(dir, OsStr::new("git"), PROBE_TIMEOUT).render_line()
    }

    fn provision_both(claude: &Path) {
        crate::fleet_commands::provision_fleet_commands_into(
            &claude.join("commands"),
            &AgentCommandRegistry::new(),
        )
        .expect("provision commands");
        crate::fleet_skills::provision_fleet_skills_into(
            &claude.join("skills"),
            &AgentSkillRegistry::new(),
        )
        .expect("provision skills");
    }

    #[test]
    fn clobber_signature_is_detectable() {
        let tmp = checkout_with_bundle();
        let claude = tmp.path().join(".claude");
        let m = total();

        // The tracked-destination guard skips every file.
        provision_both(&claude);
        let head = git(tmp.path(), &["rev-parse", "HEAD"]);
        let sha12 = head.trim().get(..12).unwrap().to_string();

        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped=0]"
            )),
            "{line}"
        );
        assert!(line.contains(&format!("@{sha12} ")), "{line}");
        assert!(line.contains("dirty-claude=0]"), "{line}");
        assert!(line.contains("[cwd: same checkout]"), "{line}");

        // A peer edits one file.
        let (edited_rel, edited_body) = bundled_files().into_iter().next().unwrap();
        let edited = claude.join(&edited_rel);
        std::fs::write(&edited, format!("{edited_body}\npeer edit\n")).unwrap();
        let line = line_for(tmp.path());
        assert!(line.contains(&format!("[bundle: {}/{m} ", m - 1)), "{line}");
        assert!(line.contains("dirty-claude=1]"), "{line}");

        // Revert it, and clobber a DIFFERENT file the way a pre-guard
        // provisioner would: the build's body plus its provenance key.
        std::fs::write(&edited, &edited_body).unwrap();
        let (name, body) = crate::fleet_commands::FLEET_COMMANDS[1];
        std::fs::write(
            claude.join(format!("commands/{name}.md")),
            crate::fleet_commands::with_provenance(name, body, CommandSource::Builtin),
        )
        .unwrap();
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped=1]"
            )),
            "a stamped clobber is still this build's bundle, and says so: {line}"
        );
        assert!(line.contains("dirty-claude=1]"), "{line}");
    }

    #[test]
    fn peer_wip_is_not_a_clobber() {
        let tmp = checkout_with_bundle();
        for (rel, body) in bundled_files() {
            std::fs::write(
                tmp.path().join(".claude").join(rel),
                format!("{body}\npeer\n"),
            )
            .unwrap();
        }
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: 0/{} identical-to-build {BUILD_SHA} stamped=0]",
                total()
            )),
            "{line}"
        );
        assert!(
            line.contains(&format!("dirty-claude={}]", total())),
            "{line}"
        );
    }

    #[test]
    fn untracked_destination_after_provision_is_healthy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_not_in_any_repo(tmp.path());
        provision_both(&tmp.path().join(".claude"));
        let m = total();
        let commands = crate::fleet_commands::FLEET_COMMANDS.len();
        let line = line_for(tmp.path());
        assert!(
            line.contains(&format!(
                "[bundle: {m}/{m} identical-to-build {BUILD_SHA} stamped={commands}]"
            )),
            "{line}"
        );
        assert!(
            line.contains("[checkout: none (not a git work tree)]"),
            "{line}"
        );
        assert!(line.contains("[cwd: same checkout]"), "{line}");
    }

    #[test]
    fn a_missing_claude_dir_is_unknown_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let line = line_for(tmp.path());
        assert!(
            line.starts_with("[served-corpus: UNKNOWN (absent)] "),
            "{line}"
        );
        assert!(
            line.contains("[checkout: UNKNOWN (no served corpus)]"),
            "{line}"
        );
        assert!(line.contains(&format!("[bundle: 0/{} ", total())), "{line}");
    }

    #[test]
    fn a_missing_git_binary_is_unknown_but_the_bundle_is_still_measured() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let nowhere = tmp.path().join("no-such-git");
        let line = probe_with(tmp.path(), nowhere.as_os_str(), PROBE_TIMEOUT).render_line();
        assert!(
            line.contains("[checkout: UNKNOWN (git unavailable)]"),
            "{line}"
        );
        assert!(line.contains("[cwd: UNKNOWN (git unavailable)]"), "{line}");
        assert!(line.contains(&format!("[bundle: 0/{} ", total())), "{line}");
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_git_times_out_once_and_bounds_the_whole_probe() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let script = tmp.path().join("slow-git");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 5\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A concurrently forking test thread can briefly hold the script's
        // write descriptor (ETXTBSY). Wait that window out before timing.
        for _ in 0..100 {
            match Command::new(&script).stdout(Stdio::null()).spawn() {
                Ok(mut c) => {
                    let _ = c.kill();
                    let _ = c.wait();
                    break;
                }
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("cannot exec the test script: {e}"),
            }
        }

        let timeout = Duration::from_millis(200);
        let started = Instant::now();
        let line = probe_with(tmp.path(), script.as_os_str(), timeout).render_line();
        let elapsed = started.elapsed();
        assert!(
            line.contains("[checkout: UNKNOWN (git timed out after 200ms)]"),
            "{line}"
        );
        assert!(
            line.contains("[cwd: UNKNOWN (git timed out after 200ms)]"),
            "{line}"
        );
        assert!(
            elapsed < timeout * 2,
            "a hung git must cost one timeout, not one per question: took {elapsed:?}"
        );
    }

    #[test]
    fn never_fetches_never_writes() {
        let tmp = checkout_with_bundle();
        git(
            tmp.path(),
            &["remote", "add", "origin", "/nonexistent/qontinui-origin"],
        );
        let index = std::fs::read(tmp.path().join(".git/index")).unwrap();
        let status_before = git(tmp.path(), &["status", "--porcelain"]);

        let line = line_for(tmp.path());
        assert!(line.contains("as-of=UNKNOWN (never fetched)"), "{line}");
        assert!(
            line.contains("upstream=UNKNOWN (no origin/main ref)"),
            "{line}"
        );

        assert_eq!(git(tmp.path(), &["status", "--porcelain"]), status_before);
        assert_eq!(std::fs::read(tmp.path().join(".git/index")).unwrap(), index);
        assert!(!tmp.path().join(".git/FETCH_HEAD").exists());
    }

    #[test]
    fn behind_and_as_of_are_read_from_the_local_remote_ref() {
        let origin = checkout_with_bundle();
        git(origin.path(), &["branch", "-M", "main"]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let clone = tmp.path().join("clone");
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                &origin.path().display().to_string(),
                "clone",
            ],
        );
        git(
            origin.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "-m",
                "more",
            ],
        );
        git(&clone, &["fetch", "--quiet", "origin"]);

        let line = line_for(&clone);
        assert!(
            line.contains("upstream=origin/main behind=1 ahead=0"),
            "{line}"
        );
        assert!(!line.contains("as-of=UNKNOWN"), "{line}");
    }

    #[cfg(unix)]
    #[test]
    fn a_workdir_in_another_checkout_gets_its_own_cwd_token() {
        let corpus_repo = checkout_with_bundle();
        let work = tempfile::tempdir().expect("tempdir");
        git(work.path(), &["init", "--quiet"]);
        git(
            work.path(),
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-verify",
                "-m",
                "w",
            ],
        );
        std::fs::write(work.path().join("f.txt"), "x").unwrap();
        git(work.path(), &["add", "--", "f.txt"]);
        std::os::unix::fs::symlink(
            corpus_repo.path().join(".claude"),
            work.path().join(".claude"),
        )
        .unwrap();

        let line = line_for(work.path());
        let canonical = std::fs::canonicalize(corpus_repo.path().join(".claude")).unwrap();
        assert!(
            line.starts_with(&format!("[served-corpus: {}] ", canonical.display())),
            "{line}"
        );
        assert!(line.contains("dirty=1]"), "{line}");
        assert!(!line.contains("[cwd: same checkout]"), "{line}");
    }

    #[test]
    fn memo_agrees_within_window() {
        let tmp = checkout_with_bundle();
        let first = probe(tmp.path()).render_line();
        // A change inside the window is not seen: the argv copy rendered before
        // provisioning and the env copy rendered after must agree.
        let (rel, _) = bundled_files().into_iter().next().unwrap();
        std::fs::write(tmp.path().join(".claude").join(rel), "changed").unwrap();
        assert_eq!(probe(tmp.path()).render_line(), first);
    }

    #[test]
    fn unknown_renders_one_token() {
        assert_eq!(
            ServedCorpus::unknown("test").render_line(),
            "[served-corpus: UNKNOWN (test)]"
        );
    }

    #[test]
    fn no_rendered_value_can_close_a_token_early() {
        assert_eq!(clean("a]b[c\nd"), "a)b(c d");
    }
}
