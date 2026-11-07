//! Does libgit2 answer the served-corpus probe's four questions the way the
//! git CLI does? Measured here, token by token, on every CI leg.
//!
//! Plan
//! `2026-10-24-served-corpus-probe-git2-parity-decides-whether-terminal-create-needs-a-process`,
//! Phase 1. [`Git2`] is an in-process [`RepoQuestions`] back end; each fixture
//! below renders the probe's line once through the CLI ([`Git`]) and once
//! through [`Git2`], and compares the `[checkout: …]`, `[bundle: …]` and
//! `[cwd: …]` tokens byte for byte (D1: an internal value that changes no
//! token is not a parity failure).
//!
//! ## Declines (D1)
//!
//! [`Git2`] never renders libgit2's failure text. On a libgit2 error it
//! DECLINES that one question and the CLI answers it — exactly the composite
//! Phase 2 would ship — so a declined question can never make the tokens
//! differ; only a git2 ANSWER can. Three libgit2 results are not failures but
//! the answer itself, and map onto the CLI path's own vocabulary rather than
//! libgit2's message:
//!
//! - repository discovery `ENOTFOUND` is "not inside a git work tree" (the
//!   CLI's `not a git repository`);
//! - `HEAD` `EUNBORNBRANCH` is [`NO_COMMIT_ON_HEAD`] (the CLI's
//!   `unknown revision`);
//! - an upstream ref lookup `ENOTFOUND` is [`no_upstream_ref`] (the CLI's
//!   `unknown revision` / `bad revision`).
//!
//! Each of those is a fixture of its own (`not_a_work_tree`, `unborn_branch`,
//! and every repository without an `origin`).
//!
//! ## What a red means
//!
//! A git2 answer that changes a token fails its test, unless
//! [`KNOWN_DIVERGENCES`] names that case with a reason — and an entry whose
//! case no longer diverges fails too, so the table cannot outlive the
//! divergence it records. The table is Phase 2's input: its D4 allowlist must
//! route every case named there to the CLI.
//!
//! A fixture this machine cannot produce (no passwordless `sudo`, a git
//! without reftable, a Windows box with no symlink or junction) FAILS when
//! `CI` is set and prints its skip otherwise (served policy `testing`
//! `a-green-run-must-prove-it-ran`).
//!
//! ## Timing
//!
//! Every case prints one `git2-parity |` row: the CLI's time over the four
//! questions, its process-creation share and the rest (the CLI timed from
//! after `spawn()` returns), and git2's own time (a declined question's CLI
//! fallback excluded). Run with `--nocapture` to see the table.

use super::tests::{checkout_with_bundle, git, line_for};
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::OsString;
use std::process::{Command, Stdio};
use std::rc::Rc;

// ── The git2 back end ───────────────────────────────────────────────────────

/// One question git2 did not answer, and libgit2's reason. Never rendered.
#[derive(Debug, Clone)]
struct Decline {
    question: &'static str,
    reason: String,
}

/// [`RepoQuestions`] answered by libgit2, each declined question answered by
/// `cli` instead.
struct Git2<'a> {
    cli: &'a dyn RepoQuestions,
    /// Repositories opened so far, keyed by the directory asked about: one
    /// measurement asks up to four questions of the same directory.
    repos: RefCell<HashMap<PathBuf, Rc<git2::Repository>>>,
    declines: RefCell<Vec<Decline>>,
    answered: Cell<usize>,
    /// Time spent in libgit2, declined attempts included, CLI fallbacks not.
    own: Cell<Duration>,
}

/// libgit2's error as a decline reason: internal, never on a token.
fn lib(e: git2::Error) -> String {
    format!("{:?}/{:?}: {}", e.class(), e.code(), e.message())
}

/// `git rev-parse`'s DWIM rules, in its order (refs.c `ref_rev_parse_rules`):
/// each is a `(prefix, suffix)` around the short name.
const REV_PARSE_RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

impl<'a> Git2<'a> {
    fn new(cli: &'a dyn RepoQuestions) -> Self {
        Git2 {
            cli,
            repos: RefCell::new(HashMap::new()),
            declines: RefCell::new(Vec::new()),
            answered: Cell::new(0),
            own: Cell::new(Duration::ZERO),
        }
    }

    /// The repository around `dir`, discovered upward the way `git -C <dir>`
    /// does — `open_ext` with no flags, never `open_from_env`, so no
    /// inherited `GIT_DIR` can point it elsewhere.
    fn open(&self, dir: &Path) -> Result<Rc<git2::Repository>, git2::Error> {
        if let Some(repo) = self.repos.borrow().get(dir) {
            return Ok(Rc::clone(repo));
        }
        let repo = Rc::new(git2::Repository::open_ext(
            dir,
            git2::RepositoryOpenFlags::empty(),
            std::iter::empty::<&OsStr>(),
        )?);
        self.repos
            .borrow_mut()
            .insert(dir.to_path_buf(), Rc::clone(&repo));
        Ok(repo)
    }

    /// Answer one question in-process, or record the decline and let the CLI
    /// answer it.
    fn ask<T>(
        &self,
        question: &'static str,
        answer: impl FnOnce() -> Result<T, String>,
        cli: impl FnOnce() -> T,
    ) -> T {
        let started = Instant::now();
        let result = answer();
        self.own.set(self.own.get() + started.elapsed());
        match result {
            Ok(v) => {
                self.answered.set(self.answered.get() + 1);
                v
            }
            Err(reason) => {
                self.declines
                    .borrow_mut()
                    .push(Decline { question, reason });
                cli()
            }
        }
    }

    fn try_locate(&self, dir: &Path) -> Result<Option<Located>, String> {
        let repo = match self.open(dir) {
            Ok(r) => r,
            Err(e)
                if e.code() == git2::ErrorCode::NotFound
                    && e.class() == git2::ErrorClass::Repository =>
            {
                return Ok(None)
            }
            Err(e) => return Err(lib(e)),
        };
        let toplevel = toplevel_of(&repo)?;
        // Inside the git dir the CLI has no toplevel to show.
        let in_git_dir = match (
            std::fs::canonicalize(dir),
            std::fs::canonicalize(repo.path()),
        ) {
            (Ok(d), Ok(g)) => d.starts_with(g),
            _ => return Err("could not canonicalize the directory or its git dir".to_string()),
        };
        if in_git_dir {
            return Err("directory is inside the git dir".to_string());
        }
        let head = match repo.head() {
            Ok(r) => {
                let sha = r
                    .target()
                    .ok_or("HEAD resolved to no object id")?
                    .to_string();
                let name = r.name().ok_or("HEAD names a non-UTF-8 ref")?;
                // A detached HEAD is the HEAD reference itself.
                let branch = if name == "HEAD" {
                    None
                } else {
                    Some(abbrev_ref(&repo, name)?)
                };
                Probe::Measured(Head { branch, sha })
            }
            Err(e) if e.code() == git2::ErrorCode::UnbornBranch => {
                Probe::Unknown(NO_COMMIT_ON_HEAD.to_string())
            }
            Err(e) => return Err(lib(e)),
        };
        Ok(Some(Located {
            toplevel,
            git_dir: repo.path().to_path_buf(),
            common_dir: repo.commondir().to_path_buf(),
            head,
        }))
    }

    fn try_ahead_behind(
        &self,
        dir: &Path,
        upstream_ref: &str,
        upstream_short: &str,
    ) -> Result<Probe<Upstream>, String> {
        let repo = self.open(dir).map_err(lib)?;
        let upstream = match repo.find_reference(upstream_ref) {
            Ok(r) => r,
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                return Ok(Probe::Unknown(no_upstream_ref(upstream_short)))
            }
            Err(e) => return Err(lib(e)),
        };
        let upstream = upstream.peel_to_commit().map_err(lib)?.id();
        let head = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .map_err(lib)?
            .id();
        let (ahead, behind) = repo.graph_ahead_behind(head, upstream).map_err(lib)?;
        let count = |n: usize| u64::try_from(n).map_err(|e| e.to_string());
        Ok(Probe::Measured(Upstream {
            name: upstream_short.to_string(),
            behind: count(behind)?,
            ahead: count(ahead)?,
        }))
    }

    fn try_changed(&self, dir: &Path, scope: DirtyScope) -> Result<Probe<Vec<String>>, String> {
        let repo = self.open(dir).map_err(lib)?;
        let mut opts = git2::StatusOptions::new();
        // The CLI's `--untracked-files=no --no-renames --ignore-submodules=all`
        // and `GIT_OPTIONAL_LOCKS=0`, option for option.
        opts.show(git2::StatusShow::IndexAndWorkdir)
            .include_untracked(false)
            .include_ignored(false)
            .include_unmodified(false)
            .exclude_submodules(true)
            .renames_head_to_index(false)
            .renames_index_to_workdir(false)
            .renames_from_rewrites(false)
            .disable_pathspec_match(true)
            .update_index(false);
        if scope == DirtyScope::Subtree {
            let prefix = prefix_in(dir, &toplevel_of(&repo)?)?;
            if !prefix.is_empty() {
                opts.pathspec(prefix);
            }
        }
        let statuses = repo.statuses(Some(&mut opts)).map_err(lib)?;
        Ok(Probe::Measured(
            statuses
                .iter()
                .filter(|e| {
                    let s = e.status();
                    !s.is_empty() && !s.intersects(git2::Status::IGNORED | git2::Status::WT_NEW)
                })
                .map(|e| String::from_utf8_lossy(e.path_bytes()).into_owned())
                .collect(),
        ))
    }

    fn try_tracked(&self, dir: &Path) -> Result<HashSet<String>, String> {
        let repo = self.open(dir).map_err(lib)?;
        let prefix = prefix_in(dir, &toplevel_of(&repo)?)?;
        let under = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        let index = repo.index().map_err(lib)?;
        Ok(index
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .filter(|p| p.starts_with(&under))
            .collect())
    }
}

impl RepoQuestions for Git2<'_> {
    fn locate(&self, dir: &Path) -> Result<Option<Located>, String> {
        self.ask(
            "locate",
            || self.try_locate(dir).map(Ok),
            || self.cli.locate(dir),
        )
    }

    fn ahead_behind(
        &self,
        dir: &Path,
        upstream_ref: &str,
        upstream_short: &str,
    ) -> Probe<Upstream> {
        self.ask(
            "ahead_behind",
            || self.try_ahead_behind(dir, upstream_ref, upstream_short),
            || self.cli.ahead_behind(dir, upstream_ref, upstream_short),
        )
    }

    fn changed(&self, dir: &Path, scope: DirtyScope) -> Probe<Vec<String>> {
        self.ask(
            "changed",
            || self.try_changed(dir, scope),
            || self.cli.changed(dir, scope),
        )
    }

    fn tracked(&self, dir: &Path) -> Result<HashSet<String>, CountUnknown> {
        self.ask(
            "tracked",
            || self.try_tracked(dir).map(Ok),
            || self.cli.tracked(dir),
        )
    }
}

/// The canonical work tree, as the CLI path canonicalizes `--show-toplevel`.
fn toplevel_of(repo: &git2::Repository) -> Result<PathBuf, String> {
    let w = repo.workdir().ok_or("bare repository")?;
    Ok(std::fs::canonicalize(w).unwrap_or_else(|_| w.components().collect()))
}

/// `dir` relative to `toplevel`, `/`-separated: the CLI's `-- .` from `dir`.
fn prefix_in(dir: &Path, toplevel: &Path) -> Result<String, String> {
    let dir = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    let rel = dir
        .strip_prefix(toplevel)
        .map_err(|_| "directory is outside its work tree".to_string())?;
    let rel = rel.to_str().ok_or("non-UTF-8 directory")?;
    Ok(rel.replace('\\', "/"))
}

/// `git rev-parse --abbrev-ref` of a full ref name: refs.c
/// `refs_shorten_unambiguous_ref`, strict unless `core.warnAmbiguousRefs` is
/// false. A branch `foo` beside a tag `foo` is `heads/foo` — which libgit2's
/// `shorthand()` would never say.
fn abbrev_ref(repo: &git2::Repository, full: &str) -> Result<String, String> {
    let strict = match repo
        .config()
        .and_then(|c| c.get_bool("core.warnAmbiguousRefs"))
    {
        Ok(b) => b,
        Err(e) if e.code() == git2::ErrorCode::NotFound => true,
        Err(e) => return Err(lib(e)),
    };
    for i in (1..REV_PARSE_RULES.len()).rev() {
        let (pre, suf) = REV_PARSE_RULES[i];
        let Some(short) = full.strip_prefix(pre).and_then(|r| r.strip_suffix(suf)) else {
            continue;
        };
        if short.is_empty() {
            continue;
        }
        // Strict: every OTHER rule must fail to name a ref; loose: only the
        // rules before the matched one.
        let to_fail = if strict { REV_PARSE_RULES.len() } else { i };
        let mut ambiguous = false;
        for (j, (p, s)) in REV_PARSE_RULES.iter().enumerate().take(to_fail) {
            if j != i && ref_exists(repo, &format!("{p}{short}{s}"))? {
                ambiguous = true;
                break;
            }
        }
        if !ambiguous {
            return Ok(short.to_string());
        }
    }
    Ok(full.to_string())
}

/// refs.c `refs_ref_exists` (`RESOLVE_REF_READING`): the name resolves,
/// through any symref, to an existing ref. Outside `refs/` git's
/// `refname_is_safe` admits only an all-caps root ref (`ORIG_HEAD`-style).
fn ref_exists(repo: &git2::Repository, name: &str) -> Result<bool, String> {
    if !name.starts_with("refs/") && !name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
        return Ok(false);
    }
    match repo.find_reference(name).and_then(|r| r.resolve()) {
        Ok(_) => Ok(true),
        Err(e)
            if matches!(
                e.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::InvalidSpec
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(lib(e)),
    }
}

// ── The harness ─────────────────────────────────────────────────────────────

/// A git2 answer that changes a token, for Phase 2's allowlist (D4) to
/// route to the CLI.
struct KnownDivergence {
    case: &'static str,
    /// Exactly the tokens that differ; any other set fails the case.
    tokens: &'static [&'static str],
    /// How each differing token ends on the CLI line, and on the git2 line:
    /// the measured values, pinned.
    cli_ends: &'static str,
    git2_ends: &'static str,
    why: &'static str,
}

/// A case listed here MUST diverge exactly as stated: one that agrees again,
/// or differs in another token or value, fails its test.
const KNOWN_DIVERGENCES: &[KnownDivergence] = &[
    KnownDivergence {
        case: "filter_clean_driver/corpus",
        tokens: &["checkout"],
        cli_ends: "dirty-claude=0",
        git2_ends: "dirty-claude=1",
        why: "libgit2 runs no external `filter.<x>.clean` driver: it hashes the raw \
              working-tree bytes, so a file the driver rewrites reads modified where \
              the CLI, which runs the driver, reads it clean",
    },
    KnownDivergence {
        case: "filter_clean_driver/cwd",
        tokens: &["cwd"],
        cli_ends: "dirty=0",
        git2_ends: "dirty=1",
        why: "the same clean driver, over the whole tree",
    },
    KnownDivergence {
        case: "sparse_cone_index_sparse_false/cwd",
        tokens: &["cwd"],
        cli_ends: "dirty=1",
        git2_ends: "dirty=3",
        why: "a cone sparse checkout with a FULL index: over the whole tree git2 \
              counts the two skip-worktree entries outside the cone (absent from \
              disk by design) as changed. Inside the cone (`/corpus`, the \
              .claude-scoped status) the two agree",
    },
];

/// Ample for every fixture, so no CLI answer is a deadline UNKNOWN.
const PARITY_BUDGET: Duration = Duration::from_secs(120);

/// The tokens D1 compares.
const TOKENS: [&str; 3] = ["checkout", "bundle", "cwd"];

/// Environment that changes what config or which repository the CLI reads
/// and that libgit2's `open_ext` does not read. Removed from every parity CLI
/// child, so the CLI and libgit2 read the same config files; a fixture sets
/// what it tests explicitly.
const CLI_CONFIG_ENV: &[&str] = &[
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

type Env = Vec<(OsString, Option<OsString>)>;

/// The parity CLI child's environment: [`CLI_CONFIG_ENV`] removed, then
/// `extra` set.
fn cli_env(extra: &[(&str, &str)]) -> Env {
    let mut env: Env = CLI_CONFIG_ENV
        .iter()
        .map(|k| (OsString::from(k), None))
        .collect();
    env.extend(
        extra
            .iter()
            .map(|(k, v)| (OsString::from(k), Some(OsString::from(v)))),
    );
    env
}

fn parity_git(env: &Env) -> Git<'static> {
    Git::until(
        OsStr::new("git"),
        PARITY_BUDGET,
        Instant::now() + PARITY_BUDGET,
        std::process::Command::spawn,
    )
    .with_env(env.clone())
}

/// [`RepoQuestions`] with the time spent answering them recorded.
struct Timed<'a> {
    inner: &'a dyn RepoQuestions,
    spent: Cell<Duration>,
}

impl Timed<'_> {
    fn time<T>(&self, f: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let v = f();
        self.spent.set(self.spent.get() + started.elapsed());
        v
    }
}

impl RepoQuestions for Timed<'_> {
    fn locate(&self, dir: &Path) -> Result<Option<Located>, String> {
        self.time(|| self.inner.locate(dir))
    }
    fn ahead_behind(&self, dir: &Path, r: &str, s: &str) -> Probe<Upstream> {
        self.time(|| self.inner.ahead_behind(dir, r, s))
    }
    fn changed(&self, dir: &Path, scope: DirtyScope) -> Probe<Vec<String>> {
        self.time(|| self.inner.changed(dir, scope))
    }
    fn tracked(&self, dir: &Path) -> Result<HashSet<String>, CountUnknown> {
        self.time(|| self.inner.tracked(dir))
    }
}

/// One back end's rendering of a fixture, and what it cost.
struct Rendered {
    line: String,
    /// All four questions' wall time.
    total: Duration,
    /// Of `total`, CLI process creation.
    spawn: Duration,
    declines: Vec<Decline>,
    answered: usize,
}

fn render_cli(workdir: &Path, env: &Env) -> Rendered {
    let cli = parity_git(env);
    let timed = Timed {
        inner: &cli,
        spent: Cell::new(Duration::ZERO),
    };
    let line = probe_with_questions(workdir, &timed, None).render_line();
    Rendered {
        line,
        total: timed.spent.get(),
        spawn: cli.spawn_spent.get(),
        declines: Vec::new(),
        answered: 0,
    }
}

fn render_git2(workdir: &Path, env: &Env) -> Rendered {
    let fallback = parity_git(env);
    let g2 = Git2::new(&fallback);
    let line = probe_with_questions(workdir, &g2, None).render_line();
    Rendered {
        line,
        total: g2.own.get(),
        spawn: Duration::ZERO,
        declines: g2.declines.take(),
        answered: g2.answered.get(),
    }
}

/// The value of the `[<key>: …]` token. No rendered value carries a `]`.
fn token<'l>(line: &'l str, key: &str) -> Option<&'l str> {
    let open = format!("[{key}: ");
    let start = line.find(&open)? + open.len();
    let len = line.get(start..)?.find(']')?;
    line.get(start..start + len)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Which back end a case must see answer. Every fixture states one, so a
/// case can never pass because git2 quietly declined everything and the CLI
/// was compared with itself.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// libgit2 reads this repository: git2 must answer EVERY question.
    Answer,
    /// libgit2 cannot read this repository (an index extension, a ref
    /// backend, an ownership rule it cannot be told about): git2 must decline
    /// at least one question.
    Decline,
}

/// Render `workdir` through both back ends `runs` times (the minimum time
/// of each is reported), print the row, and assert D1.
fn check_timed(
    case: &str,
    workdir: &Path,
    extra_env: &[(&str, &str)],
    expect: Expect,
    runs: usize,
) {
    let env = cli_env(extra_env);
    let mut cli = render_cli(workdir, &env);
    let mut g2 = render_git2(workdir, &env);
    for _ in 1..runs {
        let c = render_cli(workdir, &env);
        if c.total < cli.total {
            cli = c;
        }
        let g = render_git2(workdir, &env);
        if g.total < g2.total {
            g2 = g;
        }
    }
    // A token missing from both lines would compare equal: every one must be
    // rendered on both.
    for k in TOKENS {
        assert!(
            token(&cli.line, k).is_some() && token(&g2.line, k).is_some(),
            "{case}: the [{k}: …] token is missing\n  cli : {}\n  git2: {}",
            cli.line,
            g2.line
        );
    }
    let differing: Vec<&str> = TOKENS
        .iter()
        .copied()
        .filter(|k| token(&cli.line, k) != token(&g2.line, k))
        .collect();
    let known = KNOWN_DIVERGENCES.iter().find(|d| d.case == case);
    let verdict = match (differing.is_empty(), known) {
        (true, None) if g2.declines.is_empty() => "identical".to_string(),
        (true, None) => "identical (git2 declined, the CLI answered)".to_string(),
        (false, Some(_)) => format!("KNOWN DIVERGENCE in {}", differing.join(",")),
        (false, None) => format!("DIVERGENT in {}", differing.join(",")),
        (true, Some(_)) => "STALE known-divergence entry".to_string(),
    };
    let declined: Vec<&str> = g2.declines.iter().map(|d| d.question).collect();
    println!(
        "git2-parity | {case:<46} | cli {:>8.2} ms (spawn {:>7.2}, post-spawn {:>7.2}) | \
         git2 {:>8.2} ms | git2 answered {} declined {} {:?} | {verdict}",
        ms(cli.total),
        ms(cli.spawn),
        ms(cli.total.saturating_sub(cli.spawn)),
        ms(g2.total),
        g2.answered,
        g2.declines.len(),
        declined,
    );
    match (differing.is_empty(), known) {
        (true, None) => {}
        (false, Some(d)) => {
            let pinned = differing == d.tokens
                && d.tokens.iter().all(|k| {
                    token(&cli.line, k).is_some_and(|v| v.ends_with(d.cli_ends))
                        && token(&g2.line, k).is_some_and(|v| v.ends_with(d.git2_ends))
                });
            assert!(
                pinned,
                "{case}: diverges, but not as KNOWN_DIVERGENCES pins it (tokens {:?}, cli \
                 ...{}, git2 ...{}); measured tokens {differing:?}\n  cli : {}\n  git2: {}",
                d.tokens, d.cli_ends, d.git2_ends, cli.line, g2.line
            );
            println!("git2-parity | {case:<46} | known divergence: {}", d.why);
        }
        (false, None) => panic!(
            "{case}: a git2 ANSWER changed {differing:?} (D1)\n  cli : {}\n  git2: {}\n  \
             git2 declines: {:?}",
            cli.line, g2.line, g2.declines
        ),
        (true, Some(_)) => panic!(
            "{case}: KNOWN_DIVERGENCES names this case but git2 now agrees with the CLI — \
             remove the entry (and re-measure Phase 2's allowlist)\n  line: {}",
            cli.line
        ),
    }
    if expect == Expect::Answer {
        assert!(
            g2.declines.is_empty(),
            "{case}: expected libgit2 to answer every question, but it declined {:?}\n  \
             line: {}",
            g2.declines,
            g2.line
        );
    }
    if expect == Expect::Decline {
        assert!(
            !g2.declines.is_empty(),
            "{case}: expected libgit2 to decline at least one question, but it answered all \
             {} — re-measure: Phase 2's allowlist assumes it cannot read this repository\n  \
             line: {}",
            g2.answered,
            g2.line
        );
    }
}

fn check(case: &str, workdir: &Path, extra_env: &[(&str, &str)], expect: Expect) {
    check_timed(case, workdir, extra_env, expect, 1);
}

/// Check `repo` twice: as the served corpus's own checkout (`<case>/corpus`,
/// the `.claude/`-scoped status), and as the `[cwd: …]` checkout of a
/// workdir inside it whose `.claude` is another checkout's
/// (`<case>/cwd`, the whole-tree status). Built before any check, so a
/// fixture that later locks the repository has both layouts already.
struct Layouts {
    repo: PathBuf,
    cwd: Option<PathBuf>,
    _corpus: tempfile::TempDir,
}

fn layouts(case: &str, repo: &Path) -> Layouts {
    let corpus = checkout_with_bundle();
    let cwd = match workdir_in(repo, corpus.path()) {
        Ok(wd) => Some(wd),
        Err(why) => {
            unproducible(&format!("{case}/cwd"), &why);
            None
        }
    };
    Layouts {
        repo: repo.to_path_buf(),
        cwd,
        _corpus: corpus,
    }
}

impl Layouts {
    fn check(&self, case: &str, extra_env: &[(&str, &str)], expect: Expect) {
        check(&format!("{case}/corpus"), &self.repo, extra_env, expect);
        if let Some(cwd) = &self.cwd {
            check(&format!("{case}/cwd"), cwd, extra_env, expect);
        }
    }
}

fn check_both(case: &str, repo: &Path) {
    layouts(case, repo).check(case, &[], Expect::Answer);
}

/// A fixture this machine cannot produce: a failure under `CI`, a printed
/// skip otherwise.
fn unproducible(case: &str, why: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "git2-parity fixture {case} could not be produced under CI: {why}"
    );
    println!("git2-parity | {case:<46} | SKIPPED (not producible here): {why}");
}

/// `<repo>/.qontinui-wd`, an untracked workdir inside `repo` whose `.claude`
/// links to `corpus_checkout`'s.
fn workdir_in(repo: &Path, corpus_checkout: &Path) -> Result<PathBuf, String> {
    let wd = repo.join(".qontinui-wd");
    std::fs::create_dir_all(&wd).map_err(|e| e.to_string())?;
    link_dir(&corpus_checkout.join(".claude"), &wd.join(".claude"))?;
    Ok(wd)
}

// `#[cfg(test)]` again on the items that spawn a process: the module is
// already test-only (its `mod` line in served_corpus.rs), but the
// process_helpers spawn guards read one file at a time and skip only items
// they can see are test-only.
/// A directory link: a symlink on Unix; on Windows a directory symlink, or a
/// junction where symlinks need a privilege the box lacks.
#[cfg(test)]
fn link_dir(target: &Path, link: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(|e| format!("symlink: {e}"))
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Ok(());
        }
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("mklink /J: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "no directory symlink, and mklink /J failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// [`git`] for a step that may legitimately fail on this machine.
#[cfg(test)]
fn try_git(dir: &Path, args: &[&str]) -> Result<String, String> {
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
        .map_err(|e| format!("run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn commit(dir: &Path, msg: &str) {
    git(
        dir,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "--no-verify",
            "-m",
            msg,
        ],
    );
}

/// Write every bundled file under `<dir>/.claude/`, stage and commit them.
fn commit_bundle_in(dir: &Path) {
    write_bundle_in(dir);
    git(dir, &["add", "--", ".claude"]);
    git(dir, &["commit", "--quiet", "--no-verify", "-m", "bundle"]);
}

fn write_bundle_in(dir: &Path) {
    for f in bundled_files() {
        let p = dir.join(".claude").join(&f.rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, f.body).unwrap();
    }
}

/// Move a file's mtime away from what the index recorded, so both back ends
/// must re-read its content.
fn touch(path: &Path) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(5))
        .unwrap();
}

/// `origin` (the bundle, on `main`, plus one commit the clone has not seen)
/// and `clone`, which has fetched it and committed once of its own:
/// behind=1 ahead=1.
fn origin_and_clone(tmp: &Path, clone_args: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let origin = checkout_with_bundle();
    git(origin.path(), &["branch", "-M", "main"]);
    commit(origin.path(), "second");
    let clone = tmp.join("clone");
    let url = if clone_args.contains(&"--depth") {
        format!("file://{}", origin.path().display())
    } else {
        origin.path().display().to_string()
    };
    let mut args = vec!["clone", "--quiet"];
    args.extend_from_slice(clone_args);
    args.extend([url.as_str(), "clone"]);
    git(tmp, &args);
    commit(origin.path(), "more");
    git(&clone, &["fetch", "--quiet", "origin"]);
    commit(&clone, "local");
    (origin, clone)
}

// ── Fixtures ────────────────────────────────────────────────────────────────

#[test]
fn plain_repo_clean() {
    let repo = checkout_with_bundle();
    assert!(line_for(repo.path()).contains("dirty-claude=0]"));
    check_both("plain_repo_clean", repo.path());
    // The `&dyn` path production takes renders what the parity CLI renders.
    let production = line_for(repo.path());
    let parity = render_cli(repo.path(), &cli_env(&[])).line;
    for k in TOKENS {
        assert_eq!(token(&production, k), token(&parity, k), "{k}");
    }
}

#[test]
fn plain_repo_dirty() {
    let repo = checkout_with_bundle();
    let dir = repo.path();
    std::fs::write(dir.join("README.md"), "readme\n").unwrap();
    git(dir, &["add", "--", "README.md"]);
    commit(dir, "readme");
    let claude = dir.join(".claude");
    let files = bundled_files();
    // An unstaged edit, a staged edit, an edit outside `.claude/`.
    std::fs::write(
        claude.join(&files[0].rel),
        format!("{}\nedit\n", files[0].body),
    )
    .unwrap();
    std::fs::write(
        claude.join(&files[3].rel),
        format!("{}\nstaged\n", files[3].body),
    )
    .unwrap();
    git(dir, &["add", "--", &format!(".claude/{}", files[3].rel)]);
    std::fs::write(dir.join("README.md"), "changed\n").unwrap();
    // A stamped TRACKED file and a stamped UNTRACKED one, so the probe asks
    // `tracked` (ls-files) too.
    for (i, untrack) in [(1, false), (2, true)] {
        let (name, body) = crate::fleet_commands::FLEET_COMMANDS[i];
        let rel = format!(".claude/commands/{name}.md");
        if untrack {
            git(dir, &["rm", "--cached", "--quiet", "--", &rel]);
        }
        std::fs::write(
            dir.join(&rel),
            crate::provenance::with_provenance(
                &crate::provenance::command_canonical(name),
                body,
                "builtin",
                None,
            ),
        )
        .unwrap();
    }
    let line = line_for(dir);
    assert!(line.contains("stamped=2 stamped-tracked=1 "), "{line}");
    check_both("plain_repo_dirty", dir);
}

#[test]
fn not_a_work_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::provision_guard::test_support::assert_not_in_any_repo(tmp.path());
    write_bundle_in(tmp.path());
    check("not_a_work_tree/corpus", tmp.path(), &[], Expect::Answer);
    // A workdir outside any repository serving another checkout's corpus.
    let corpus = checkout_with_bundle();
    let wd = tmp.path().join("wd");
    std::fs::create_dir_all(&wd).unwrap();
    match link_dir(&corpus.path().join(".claude"), &wd.join(".claude")) {
        Ok(()) => check("not_a_work_tree/cwd", &wd, &[], Expect::Answer),
        Err(why) => unproducible("not_a_work_tree/cwd", &why),
    }
}

#[test]
fn clone_with_fetched_upstream_behind_and_ahead() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_origin, clone) = origin_and_clone(tmp.path(), &[]);
    let line = line_for(&clone);
    assert!(
        line.contains("upstream=origin/main behind=1 ahead=1"),
        "{line}"
    );
    assert!(!line.contains("as-of=UNKNOWN"), "{line}");
    check_both("clone_behind_ahead", &clone);
}

#[test]
fn shallow_clone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_origin, clone) = origin_and_clone(tmp.path(), &["--depth", "1"]);
    if !clone.join(".git/shallow").exists() {
        return unproducible("shallow_clone", "the clone is not shallow");
    }
    let line = line_for(&clone);
    assert!(
        line.contains("upstream=origin/main behind=1 ahead=1"),
        "{line}"
    );
    check_both("shallow_clone", &clone);
}

#[test]
fn linked_worktree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_origin, clone) = origin_and_clone(tmp.path(), &[]);
    let wt = tmp.path().join("wt");
    git(
        &clone,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "wt",
            &wt.display().to_string(),
            "HEAD~1",
        ],
    );
    commit(&wt, "in the worktree");
    let line = line_for(&wt);
    // HEAD~1 of the clone is origin's `second`; origin/main is one past it.
    assert!(
        line.contains("upstream=origin/main behind=1 ahead=1"),
        "{line}"
    );
    assert!(!line.contains("as-of=UNKNOWN"), "{line}");
    check_both("linked_worktree", &wt);
}

#[test]
fn corpus_symlinked_from_another_checkout() {
    let corpus_repo = checkout_with_bundle();
    let work = tempfile::tempdir().expect("tempdir");
    git(work.path(), &["init", "--quiet"]);
    commit(work.path(), "w");
    std::fs::write(work.path().join("f.txt"), "x").unwrap();
    git(work.path(), &["add", "--", "f.txt"]);
    if let Err(why) = link_dir(
        &corpus_repo.path().join(".claude"),
        &work.path().join(".claude"),
    ) {
        return unproducible("corpus_symlinked_from_another_checkout", &why);
    }
    let line = line_for(work.path());
    assert!(line.contains("dirty=1]"), "the 7-process path: {line}");
    check(
        "corpus_symlinked_from_another_checkout",
        work.path(),
        &[],
        Expect::Answer,
    );
}

#[test]
fn detached_head() {
    let repo = checkout_with_bundle();
    commit(repo.path(), "second");
    git(repo.path(), &["checkout", "--quiet", "--detach", "HEAD~1"]);
    assert!(line_for(repo.path()).contains("(detached)@"));
    check_both("detached_head", repo.path());
}

#[test]
fn unborn_branch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    git(tmp.path(), &["init", "--quiet"]);
    write_bundle_in(tmp.path());
    git(tmp.path(), &["add", "--", ".claude"]);
    let line = line_for(tmp.path());
    assert!(line.contains("HEAD=UNKNOWN (no commit on HEAD)"), "{line}");
    check_both("unborn_branch", tmp.path());
}

/// `git rev-parse --abbrev-ref HEAD` says `heads/foo` when a tag `foo`
/// exists; libgit2's `shorthand()` says `foo`.
#[test]
fn branch_named_like_a_tag() {
    let repo = checkout_with_bundle();
    git(repo.path(), &["tag", "foo"]);
    git(repo.path(), &["checkout", "--quiet", "-b", "foo"]);
    let line = line_for(repo.path());
    assert!(line.contains(" heads/foo@"), "{line}");
    check_both("branch_named_like_a_tag", repo.path());
}

#[test]
fn branch_containing_a_bracket() {
    let repo = checkout_with_bundle();
    git(repo.path(), &["checkout", "--quiet", "-b", "feat]x"]);
    let line = line_for(repo.path());
    assert!(
        line.contains(" feat)x@"),
        "the `]` is cleaned to `)`: {line}"
    );
    check_both("branch_containing_a_bracket", repo.path());
}

/// A repository whose `.claude/vendor-sub` is a submodule, so both the
/// `.claude/`-scoped and the whole-tree status meet it.
fn repo_with_submodule() -> (tempfile::TempDir, tempfile::TempDir) {
    let sub = tempfile::tempdir().expect("tempdir");
    git(sub.path(), &["init", "--quiet"]);
    std::fs::write(sub.path().join("file.txt"), "sub\n").unwrap();
    git(sub.path(), &["add", "--", "file.txt"]);
    commit(sub.path(), "sub");
    let repo = checkout_with_bundle();
    git(
        repo.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            &sub.path().display().to_string(),
            ".claude/vendor-sub",
        ],
    );
    commit(repo.path(), "submodule");
    (repo, sub)
}

#[test]
fn submodule_clean() {
    let (repo, _sub) = repo_with_submodule();
    let status = git(repo.path(), &["status", "--porcelain"]);
    assert!(status.is_empty(), "{status}");
    let line = line_for(repo.path());
    assert!(line.contains("dirty-claude=0]"), "{line}");
    check_both("submodule_clean", repo.path());
}

#[test]
fn submodule_with_modified_content() {
    let (repo, _sub) = repo_with_submodule();
    std::fs::write(repo.path().join(".claude/vendor-sub/file.txt"), "edited\n").unwrap();
    // git itself sees the submodule modified; the probe ignores submodules.
    let status = git(repo.path(), &["status", "--porcelain"]);
    assert!(status.contains(" M .claude/vendor-sub"), "{status}");
    let line = line_for(repo.path());
    assert!(line.contains("dirty-claude=0]"), "{line}");
    check_both("submodule_modified_content", repo.path());
}

#[test]
fn submodule_with_a_staged_gitlink_change() {
    let (repo, _sub) = repo_with_submodule();
    let inner = repo.path().join(".claude/vendor-sub");
    commit(&inner, "moved");
    git(repo.path(), &["add", "--", ".claude/vendor-sub"]);
    let status = git(repo.path(), &["status", "--porcelain"]);
    assert!(status.contains("M  .claude/vendor-sub"), "{status}");
    let line = line_for(repo.path());
    assert!(line.contains("dirty-claude=0]"), "{line}");
    check_both("submodule_staged_gitlink", repo.path());
}

#[test]
fn non_ascii_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("répertoire-ü-корпус");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet"]);
    write_bundle_in(&dir);
    std::fs::write(dir.join(".claude/ñandú.md"), "a\n").unwrap();
    git(&dir, &["add", "--", ".claude"]);
    commit(&dir, "bundle");
    std::fs::write(dir.join(".claude/ñandú.md"), "changed\n").unwrap();
    let line = line_for(&dir);
    assert!(line.contains("[checkout: répertoire-ü-корпус "), "{line}");
    assert!(line.contains("dirty-claude=1]"), "{line}");
    check_both("non_ascii_path", &dir);
}

/// A cone sparse checkout of `.claude` with `outside/` skipped, and one
/// `.claude/` edit.
fn sparse_repo(case: &str, sparse_index: bool) -> Option<tempfile::TempDir> {
    let repo = checkout_with_bundle();
    let dir = repo.path();
    std::fs::create_dir_all(dir.join("outside/deep")).unwrap();
    std::fs::write(dir.join("outside/a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("outside/deep/b.txt"), "b\n").unwrap();
    git(dir, &["add", "--", "outside"]);
    commit(dir, "outside");
    let index_flag = if sparse_index {
        "--sparse-index"
    } else {
        "--no-sparse-index"
    };
    if let Err(why) = try_git(
        dir,
        &["sparse-checkout", "set", "--cone", index_flag, ".claude"],
    ) {
        unproducible(case, &why);
        return None;
    }
    if dir.join("outside/a.txt").exists() {
        unproducible(case, "sparse-checkout left outside/ on disk");
        return None;
    }
    let expected = if sparse_index { "true" } else { "false" };
    let got = try_git(dir, &["config", "index.sparse"]).unwrap_or_default();
    if got.trim() != expected {
        unproducible(case, &format!("index.sparse is {got:?}, not {expected}"));
        return None;
    }
    let first = &bundled_files()[0];
    std::fs::write(dir.join(".claude").join(&first.rel), "edited\n").unwrap();
    Some(repo)
}

#[test]
fn sparse_checkout_cone_full_index() {
    let case = "sparse_cone_index_sparse_false";
    if let Some(repo) = sparse_repo(case, false) {
        let line = line_for(repo.path());
        assert!(line.contains("dirty-claude=1]"), "{line}");
        check_both(case, repo.path());
    }
}

#[test]
fn sparse_checkout_cone_sparse_index_is_declined() {
    let case = "sparse_cone_index_sparse_true";
    if let Some(repo) = sparse_repo(case, true) {
        layouts(case, repo.path()).check(case, &[], Expect::Decline);
    }
}

#[test]
fn split_index_is_declined() {
    let case = "core_split_index";
    let repo = checkout_with_bundle();
    git(repo.path(), &["config", "core.splitIndex", "true"]);
    git(repo.path(), &["update-index", "--split-index"]);
    let shared = std::fs::read_dir(repo.path().join(".git"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().starts_with("sharedindex."));
    if !shared {
        return unproducible(case, "no sharedindex.* after --split-index");
    }
    layouts(case, repo.path()).check(case, &[], Expect::Decline);
}

#[test]
fn reftable_is_declined() {
    let case = "reftable";
    let tmp = tempfile::tempdir().expect("tempdir");
    if let Err(why) = try_git(tmp.path(), &["init", "--quiet", "--ref-format=reftable"]) {
        return unproducible(case, &format!("this git has no reftable: {why}"));
    }
    commit_bundle_in(tmp.path());
    layouts(case, tmp.path()).check(case, &[], Expect::Decline);
}

#[test]
fn autocrlf_crlf_working_copy_after_touch() {
    let case = "autocrlf_crlf_after_touch";
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    git(dir, &["init", "--quiet"]);
    git(dir, &["config", "core.autocrlf", "true"]);
    commit_bundle_in(dir);
    // Re-check out, so the working copy is CRLF and the index's stat data
    // describes the CRLF files.
    std::fs::remove_dir_all(dir.join(".claude")).unwrap();
    git(dir, &["checkout", "--", ".claude"]);
    let first = dir.join(".claude").join(&bundled_files()[0].rel);
    if !std::fs::read_to_string(&first).unwrap().contains("\r\n") {
        return unproducible(case, "core.autocrlf=true did not check out CRLF");
    }
    for f in bundled_files().iter().take(5) {
        touch(&dir.join(".claude").join(&f.rel));
    }
    let line = line_for(dir);
    assert!(
        line.contains("dirty-claude=0]"),
        "CRLF of LF blobs is clean: {line}"
    );
    check_both(case, dir);
}

#[test]
fn filter_clean_driver() {
    let case = "filter_clean_driver";
    let repo = checkout_with_bundle();
    let dir = repo.path();
    git(
        dir,
        &["config", "filter.qontinui-strip.clean", "sed -e s/LOCAL/x/"],
    );
    std::fs::write(
        dir.join(".claude/.gitattributes"),
        "filtered.txt filter=qontinui-strip\n",
    )
    .unwrap();
    std::fs::write(dir.join(".claude/filtered.txt"), "LOCAL\n").unwrap();
    git(
        dir,
        &[
            "add",
            "--",
            ".claude/.gitattributes",
            ".claude/filtered.txt",
        ],
    );
    commit(dir, "filtered");
    let blob = git(dir, &["cat-file", "-p", "HEAD:.claude/filtered.txt"]);
    if blob != "x\n" {
        return unproducible(
            case,
            &format!("the clean driver did not run: blob {blob:?}"),
        );
    }
    touch(&dir.join(".claude/filtered.txt"));
    let line = line_for(dir);
    assert!(
        line.contains("dirty-claude=0]"),
        "the driver makes it clean: {line}"
    );
    check_both(case, dir);
}

/// An honest `core.fsmonitor` hook (protocol 2) that reports everything may
/// have changed, and logs each call so the fixture proves git asked it.
#[test]
fn fsmonitor_hook_script() {
    let case = "fsmonitor_hook_script";
    let tmp = tempfile::tempdir().expect("tempdir");
    let hooks = tempfile::tempdir().expect("tempdir");
    let log = hooks.path().join("calls.log");
    let hook = hooks.path().join("fsmonitor-hook");
    // Git runs `core.fsmonitor` through `sh -c`, which eats a Windows
    // backslash: both paths it reads are spelled with forward slashes.
    let slashed = |p: &Path| p.display().to_string().replace('\\', "/");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{}'\nprintf 'qontinui-token\\0/\\0'\n",
        slashed(&log)
    );
    #[cfg(unix)]
    crate::canonical_corpus::test_support::executable_script(&hook, &body);
    #[cfg(windows)]
    std::fs::write(&hook, &body).unwrap();
    let dir = tmp.path();
    git(dir, &["init", "--quiet"]);
    git(dir, &["config", "core.fsmonitor", &slashed(&hook)]);
    git(dir, &["config", "core.fsmonitorHookVersion", "2"]);
    commit_bundle_in(dir);
    let first = &bundled_files()[0];
    std::fs::write(dir.join(".claude").join(&first.rel), "edited\n").unwrap();
    let line = line_for(dir);
    assert!(line.contains("dirty-claude=1]"), "{line}");
    // Cleared now, so a non-empty log below proves the parity runs asked it.
    let _ = std::fs::remove_file(&log);
    check_both(case, dir);
    if std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0) == 0 {
        unproducible(case, "git never invoked the fsmonitor hook");
    }
}

/// The built-in daemon (`core.fsmonitor=true`) exists on Windows and macOS;
/// the plan measures it on the Windows leg.
#[cfg(windows)]
#[test]
fn fsmonitor_builtin_daemon() {
    let case = "fsmonitor_builtin_daemon";
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    git(dir, &["init", "--quiet"]);
    git(dir, &["config", "core.fsmonitor", "true"]);
    struct Stop<'a>(&'a Path);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            let _ = try_git(self.0, &["fsmonitor--daemon", "stop"]);
        }
    }
    let _stop = Stop(dir);
    if let Err(why) = try_git(dir, &["fsmonitor--daemon", "start"]) {
        return unproducible(case, &why);
    }
    commit_bundle_in(dir);
    let first = &bundled_files()[0];
    std::fs::write(dir.join(".claude").join(&first.rel), "edited\n").unwrap();
    check_both(case, dir);
}

/// A directory owned by another principal for the guard's lifetime, its
/// owner restored on drop so the tempdir can be removed.
struct ForeignOwner {
    dir: PathBuf,
    #[cfg(unix)]
    restore: String,
}

#[cfg(test)]
impl ForeignOwner {
    #[cfg(unix)]
    fn take(dir: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(dir).map_err(|e| e.to_string())?;
        let restore = format!("{}:{}", meta.uid(), meta.gid());
        let out = Command::new("sudo")
            .args(["-n", "chown", "0:0"])
            .arg(dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("sudo: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "no passwordless sudo chown: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(ForeignOwner {
            dir: dir.to_path_buf(),
            restore,
        })
    }

    #[cfg(windows)]
    fn take(dir: &Path) -> Result<Self, String> {
        let out = Command::new("icacls")
            .arg(dir)
            .args(["/setowner", "*S-1-5-18"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("icacls: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "icacls /setowner SYSTEM failed (not elevated?): {}",
                String::from_utf8_lossy(&out.stdout).trim()
            ));
        }
        Ok(ForeignOwner {
            dir: dir.to_path_buf(),
        })
    }
}

#[cfg(test)]
impl Drop for ForeignOwner {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = Command::new("sudo")
            .args(["-n", "chown", &self.restore])
            .arg(&self.dir)
            .stdin(Stdio::null())
            .output();
        #[cfg(windows)]
        {
            if let Ok(user) = std::env::var("USERNAME") {
                let _ = Command::new("icacls")
                    .arg(&self.dir)
                    .args(["/setowner", &user])
                    .stdin(Stdio::null())
                    .output();
            }
        }
    }
}

/// What `git rev-parse --show-toplevel` in `dir` says under `extra` env.
fn cli_rev_parse(dir: &Path, extra: &[(&str, &str)]) -> Result<String, String> {
    let mut cmd = crate::process_helpers::scrubbed_git(OsStr::new("git"));
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"]);
    for (k, v) in cli_env(extra) {
        match v {
            Some(v) => cmd.env(k, v),
            None => cmd.env_remove(k),
        };
    }
    cmd.stdin(Stdio::null());
    let out = crate::process_helpers::output_with_timeout(cmd, std::time::Duration::from_secs(60))
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// `safe.directory=*` as the runner's environment would carry it, through
/// git's `GIT_CONFIG_COUNT` overlay (`git_posture.rs`), which libgit2 does
/// not read.
const SAFE_DIRECTORY_INJECTED: &[(&str, &str)] = &[
    ("GIT_CONFIG_COUNT", "1"),
    ("GIT_CONFIG_KEY_0", "safe.directory"),
    ("GIT_CONFIG_VALUE_0", "*"),
];

/// Both owner-mismatch cases: the repository's top directory owned by root
/// (Unix) or SYSTEM (Windows) — never `GIT_TEST_ASSUME_DIFFERENT_OWNER`,
/// which only git honours.
fn owner_mismatch(case: &str, extra: &[(&str, &str)]) {
    let repo = checkout_with_bundle();
    let both = layouts(case, repo.path());
    let _owner = match ForeignOwner::take(repo.path()) {
        Ok(o) => o,
        Err(why) => return unproducible(case, &why),
    };
    match cli_rev_parse(repo.path(), &[]) {
        Err(e) if e.contains("dubious ownership") => {}
        other => {
            return unproducible(
                case,
                &format!(
                    "the CLI does not see a dubious owner (a global safe.directory?): {other:?}"
                ),
            )
        }
    }
    if !extra.is_empty() {
        if let Err(e) = cli_rev_parse(repo.path(), extra) {
            return unproducible(
                case,
                &format!("the injected safe.directory did not take: {e}"),
            );
        }
    }
    both.check(case, extra, Expect::Decline);
}

#[test]
fn owner_mismatch_is_declined() {
    owner_mismatch("owner_mismatch", &[]);
}

/// The CLI reads the injected `safe.directory` and answers; libgit2 cannot be
/// told it and must decline, never answer from a repository it refused.
#[test]
fn owner_mismatch_with_injected_safe_directory_is_declined() {
    owner_mismatch(
        "owner_mismatch_safe_directory_injected",
        SAFE_DIRECTORY_INJECTED,
    );
}

/// The timing case: a `[cwd: …]` checkout of >= 20k tracked files, measured
/// over its whole tree twice — first as a fresh checkout leaves it (every
/// entry written in the same second as the index, so racily clean and
/// re-read by content), then aged past the index and refreshed, the
/// day-to-day state. Each row is the best of three runs per back end.
#[test]
fn large_tree_whole_tree_cwd() {
    let case = "large_tree_20k_whole_tree_cwd";
    const DIRS: usize = 200;
    const PER_DIR: usize = 101;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    git(dir, &["init", "--quiet"]);
    for d in 0..DIRS {
        let sub = dir.join(format!("d{d:03}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..PER_DIR {
            std::fs::write(sub.join(format!("f{f:03}.txt")), format!("{d} {f}\n")).unwrap();
        }
    }
    git(dir, &["add", "--", "."]);
    commit(dir, "large");
    let tracked = git(dir, &["ls-files", "-z"])
        .split('\0')
        .filter(|p| !p.is_empty())
        .count();
    assert!(tracked >= 20_000, "{tracked} tracked files");
    std::fs::write(dir.join("d000/f000.txt"), "edited\n").unwrap();
    let corpus = checkout_with_bundle();
    let wd = match workdir_in(dir, corpus.path()) {
        Ok(wd) => wd,
        Err(why) => return unproducible(case, &why),
    };
    check_timed(&format!("{case}/fresh"), &wd, &[], Expect::Answer, 3);
    std::thread::sleep(Duration::from_millis(1100));
    // A refresh re-stats every entry and writes the index; it stages nothing,
    // so the one edit stays changed.
    let _ = try_git(dir, &["update-index", "-q", "--refresh"]);
    check_timed(&format!("{case}/aged"), &wd, &[], Expect::Answer, 3);
}
