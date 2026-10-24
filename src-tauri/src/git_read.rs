//! Read-only git facts answered in-process with `git2`, behind a CLI fallback.
//!
//! Plan `2026-10-24-runner-git-probes-spawn-40-processes-per-second-and-fetch-into-empty-dirs`
//! (Phase 4, design decision D2). Every runner `git` call on Windows costs
//! three processes (the `cmd\git.exe` launcher, its conhost and the real git),
//! and the census and tree publisher asked the same handful of read-only
//! questions hundreds of times a minute. These are answered here from the
//! repository on disk, without a process:
//!
//! | fact | the CLI call it replaces |
//! |---|---|
//! | [`head_branch`] | `symbolic-ref --short HEAD` |
//! | [`head_oid`] | `rev-parse HEAD` |
//! | [`head_commit`] | `log -1 --format=%ct%n%B` (and `%cI`) |
//! | [`verify_ref`] | `rev-parse --verify --quiet <ref>` |
//! | [`is_ancestor`] | `merge-base --is-ancestor <a> <b>` |
//! | [`ahead_behind`] | `rev-list --count [--left-right] <a>...<b>` / `<a>..<b>` |
//! | [`resolve_trunk_ref`] | `git_trunk`'s `symbolic-ref` + `rev-parse --verify` rungs |
//!
//! ## The contract: an answer the CLI would give, or `Err`
//!
//! Each function returns `Ok` only with the answer the CLI gives for the same
//! repository, and `Err` for ANY git2 error and for every shape whose CLI
//! answer it does not reproduce (an ambiguous short ref name, a commit message
//! in a non-UTF-8 encoding, replaced or grafted history, a shallow clone). The
//! caller then runs today's CLI call ([`or_cli`]), so the worst case is
//! today's behaviour. A ref that does not exist is an ANSWER, not an error: it
//! is the same verdict the CLI reports by exiting non-zero, and treating it as
//! a fallback would spawn the CLI for every local-only branch.
//!
//! The parity is measured, not assumed: the tests below run each fact through
//! both paths on fixture repositories (a clone, a linked worktree, a detached
//! HEAD, packed refs only, a `master` trunk, a missing and a stale
//! `origin/HEAD`, an empty commit message and a multi-paragraph body with a
//! `Session-Id:` trailer) and require identical answers.
//!
//! ## What stays on the CLI
//!
//! `status`, `ls-files --others` and `cherry` (D2): their semantics — ignore
//! rules, the dirty probe's carve-outs, patch ids — are what reclaim's G1 gate
//! and coord's pull verdicts read, and nothing here proves parity for them.
//!
//! ## Opening
//!
//! [`open`] is `git2::Repository::open(path)`: exactly that directory, no
//! upward discovery and no `GIT_DIR` / `GIT_WORK_TREE` from the environment
//! (never `open_from_env`), so an inherited repository-local variable cannot
//! answer for another repository — the property `git_posture`'s scrub gives the
//! CLI. A directory git would discover a PARENT repository from fails to open
//! here and falls back to the CLI, which answers for the parent as it always
//! did. libgit2's owner validation can refuse a repository the CLI opens (the
//! CLI may carry an env-injected `safe.directory`); that is an open error and
//! falls back too. Callers open ONCE per census row or publisher directory and
//! pass the `Repository` to every fact.
//!
//! **Bin target only**, like `git_trunk`: its callers (`agent_worktree`,
//! `fleet`, `mcp`) are declared in `main.rs`.

use std::path::Path;

use git2::{ErrorCode, Oid, Repository};
use tracing::{debug, info};

/// A read's answer, or the reason to ask the CLI instead.
pub(crate) type Read<T> = Result<T, git2::Error>;

/// Open the repository at exactly `path`. See the module note.
pub(crate) fn open(path: &Path) -> Read<Repository> {
    Repository::open(path)
}

/// An `Err` for a shape the CLI answers differently than a plain read would.
fn unsupported(what: &str) -> git2::Error {
    git2::Error::from_str(&format!("git_read: {what} — answered by the CLI"))
}

/// `NotFound` from a ref lookup is an answer (`None`); anything else is `Err`.
fn found<T>(r: Result<T, git2::Error>) -> Read<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.code() == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// History the CLI reads differently than libgit2: `refs/replace/*` (git
/// substitutes the replacement object; libgit2 does not), `info/grafts`, and a
/// shallow clone's cut-off parents. Any of them sends commit reads to the CLI.
fn plain_history(repo: &Repository) -> Read<()> {
    if repo.is_shallow() {
        return Err(unsupported("shallow repository"));
    }
    if repo.commondir().join("info").join("grafts").exists() {
        return Err(unsupported("info/grafts present"));
    }
    if repo.references_glob("refs/replace/*")?.next().is_some() {
        return Err(unsupported("refs/replace present"));
    }
    Ok(())
}

/// git's `shorten_unambiguous_ref` for the two namespaces these reads meet
/// (`refs/heads/`, `refs/remotes/`), as `symbolic-ref --short` prints it. git
/// picks the most specific rule that matches and keeps it only when no
/// EARLIER rule (`%s`, `refs/%s`, `refs/tags/%s`, and for a remote also
/// `refs/heads/%s`) names an existing ref; otherwise it prints a longer form
/// (`heads/main`, `remotes/origin/main`). An ambiguous name, any other
/// namespace, and a top-level-ref-shaped name (all caps / `_`, which git does
/// read as `$GIT_DIR/<name>`) are `Err`: the CLI answers those.
fn shorten_unambiguous(repo: &Repository, full: &str) -> Read<String> {
    let (short, earlier): (&str, &[&str]) = if let Some(s) = full.strip_prefix("refs/heads/") {
        (s, &["refs/", "refs/tags/"])
    } else if let Some(s) = full.strip_prefix("refs/remotes/") {
        if s.ends_with("/HEAD") {
            return Err(unsupported("a remote HEAD symref target"));
        }
        (s, &["refs/", "refs/tags/", "refs/heads/"])
    } else {
        return Err(unsupported(
            "a symref target outside refs/heads and refs/remotes",
        ));
    };
    if short.is_empty() {
        return Err(unsupported("an empty ref name"));
    }
    if short.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
        return Err(unsupported("a top-level-ref-shaped name"));
    }
    for prefix in earlier {
        if found(repo.find_reference(&format!("{prefix}{short}")))?.is_some() {
            return Err(unsupported("an ambiguous short ref name"));
        }
    }
    Ok(short.to_string())
}

/// True for a spec git could read as an object id rather than a ref name.
fn hex_shaped(spec: &str) -> bool {
    !spec.is_empty() && spec.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The object a ref NAME resolves to, by git's DWIM rules (`%s`, `refs/%s`,
/// `refs/tags/%s`, `refs/heads/%s`, `refs/remotes/%s`, `refs/remotes/%s/HEAD`
/// — libgit2's `git_reference_dwim` uses the same list in the same order),
/// WITHOUT looking the object up: `rev-parse --verify` prints a dangling ref's
/// target too. `None` when no rule names a ref. A hex-shaped spec is `Err`:
/// git may read it as an (abbreviated) object id.
fn resolve_ref(repo: &Repository, spec: &str) -> Read<Option<Oid>> {
    if hex_shaped(spec) {
        return Err(unsupported("a hex-shaped revision"));
    }
    let Some(r) = found(repo.resolve_reference_from_short_name(spec))? else {
        return Ok(None);
    };
    match r.target() {
        Some(oid) => Ok(Some(oid)),
        None => Err(unsupported("a ref that did not resolve to an object id")),
    }
}

/// The COMMIT a revision names, for the commit-walking reads: a full 40-hex
/// object id as itself (it must be a commit in this repository, else `Err`),
/// otherwise a ref name through [`resolve_ref`], peeled to its commit. `None`
/// only when no ref of that name exists.
fn resolve_commit(repo: &Repository, spec: &str) -> Read<Option<Oid>> {
    if spec.len() == 40 && hex_shaped(spec) {
        let oid = Oid::from_str(spec)?;
        return Ok(Some(repo.find_commit(oid)?.id()));
    }
    let Some(oid) = resolve_ref(repo, spec)? else {
        return Ok(None);
    };
    Ok(Some(repo.find_object(oid, None)?.peel_to_commit()?.id()))
}

/// `git symbolic-ref --short HEAD`: the checked-out branch, or `None` when
/// HEAD is detached (the CLI exits non-zero there). An unborn branch still has
/// its name, as on the CLI.
pub(crate) fn head_branch(repo: &Repository) -> Read<Option<String>> {
    let head = repo.find_reference("HEAD")?;
    let Some(target) = head.symbolic_target_bytes() else {
        return Ok(None);
    };
    let target = std::str::from_utf8(target).map_err(|_| unsupported("a non-UTF-8 HEAD target"))?;
    shorten_unambiguous(repo, target).map(Some)
}

/// `git rev-parse HEAD`: the commit HEAD names, or `None` on an unborn branch
/// (the CLI fails there).
pub(crate) fn head_oid(repo: &Repository) -> Read<Option<String>> {
    match repo.head() {
        Ok(h) => match h.target() {
            Some(oid) => Ok(Some(oid.to_string())),
            None => Err(unsupported("HEAD did not resolve to an object id")),
        },
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// HEAD's commit as the census and publisher read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadCommit {
    /// `%ct` — the committer timestamp, seconds since the epoch.
    pub committed_epoch: i64,
    /// `%B` as `git log -1 --format=%ct%n%B` leaves it once the census trims
    /// the capture and splits off the timestamp line: the raw message with its
    /// trailing whitespace removed.
    pub body: String,
}

/// `git log -1 --format=%ct%n%B` (and `%cI`, the same timestamp): `None` on an
/// unborn branch. A message in a declared non-UTF-8 encoding is `Err`, since
/// `git log` re-encodes it.
pub(crate) fn head_commit(repo: &Repository) -> Read<Option<HeadCommit>> {
    plain_history(repo)?;
    let head = match repo.head() {
        Ok(h) => h,
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    let commit = head.peel_to_commit()?;
    if let Some(enc) = commit.message_encoding() {
        if !enc.eq_ignore_ascii_case("utf-8") && !enc.eq_ignore_ascii_case("utf8") {
            return Err(unsupported("a commit message in a non-UTF-8 encoding"));
        }
    }
    let raw = String::from_utf8_lossy(commit.message_raw_bytes());
    Ok(Some(HeadCommit {
        committed_epoch: commit.time().seconds(),
        body: raw.trim_end().to_string(),
    }))
}

/// `git rev-parse --verify --quiet <name>` for a ref NAME: the object id it
/// names, or `None` when it names no ref.
pub(crate) fn verify_ref(repo: &Repository, name: &str) -> Read<Option<String>> {
    Ok(resolve_ref(repo, name)?.map(|o| o.to_string()))
}

/// `git merge-base --is-ancestor <a> <b>`: is `a` reachable from `b`? A commit
/// IS its own ancestor here, as on the CLI (libgit2's `descendant_of` says it
/// is not, so equality is checked first). A revision that does not resolve is
/// `Err`: the census treats that CLI failure as its own path.
pub(crate) fn is_ancestor(repo: &Repository, a: &str, b: &str) -> Read<bool> {
    plain_history(repo)?;
    let (Some(a), Some(b)) = (resolve_commit(repo, a)?, resolve_commit(repo, b)?) else {
        return Err(unsupported("an unresolvable revision"));
    };
    Ok(a == b || repo.graph_descendant_of(b, a)?)
}

/// `(|a \ b|, |b \ a|)` — `git rev-list --count --left-right <a>...<b>` prints
/// it as `<left>\t<right>`; `rev-list --count <a>..<b>` is the second number.
/// `None` when either side names no ref (the CLI fails there).
pub(crate) fn ahead_behind(repo: &Repository, a: &str, b: &str) -> Read<Option<(usize, usize)>> {
    plain_history(repo)?;
    let (Some(a), Some(b)) = (resolve_commit(repo, a)?, resolve_commit(repo, b)?) else {
        return Ok(None);
    };
    Ok(Some(repo.graph_ahead_behind(a, b)?))
}

/// `git_trunk::resolve_trunk_ref`'s rungs over an open repository: (1)
/// `refs/remotes/origin/HEAD`'s target, shortened as `symbolic-ref --short`
/// prints it and verified to resolve; (2) `origin/main`, verified; (3) `None`.
/// Same order and the same verification as the CLI rungs — a stale
/// `origin/HEAD` naming a deleted branch falls through, never trusted.
pub(crate) fn resolve_trunk_ref(repo: &Repository) -> Read<Option<String>> {
    if let Some(head) = found(repo.find_reference("refs/remotes/origin/HEAD"))? {
        if let Some(target) = head.symbolic_target_bytes() {
            let target = std::str::from_utf8(target)
                .map_err(|_| unsupported("a non-UTF-8 origin/HEAD target"))?;
            let short = shorten_unambiguous(repo, target)?;
            if resolve_ref(repo, &short)?.is_some() {
                return Ok(Some(short));
            }
        }
    }
    if resolve_ref(repo, "origin/main")?.is_some() {
        return Ok(Some("origin/main".to_string()));
    }
    Ok(None)
}

/// How many facts were answered in-process and how many fell back to the CLI.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tally {
    pub in_process: usize,
    pub fell_back: usize,
}

impl Tally {
    pub(crate) fn add(&mut self, other: Tally) {
        self.in_process += other.in_process;
        self.fell_back += other.fell_back;
    }
}

/// One fact: `read` over `repo` when it opened and answered, else `cli` —
/// today's call, unchanged. Every fallback is counted in `tally`.
pub(crate) fn or_cli<T>(
    repo: Option<&Repository>,
    tally: &mut Tally,
    read: impl FnOnce(&Repository) -> Read<T>,
    cli: impl FnOnce() -> T,
) -> T {
    if let Some(r) = repo {
        match read(r) {
            Ok(v) => {
                tally.in_process += 1;
                return v;
            }
            Err(e) => debug!("git_read: {} — {e}; asking the CLI", r.path().display()),
        }
    }
    tally.fell_back += 1;
    cli()
}

/// Log a pass's [`Tally`] once (the census per walk, the publisher per cycle):
/// at INFO when anything fell back, so a box where git2 never answers — owner
/// validation, an unsupported extension — is visible; at DEBUG otherwise.
pub(crate) fn report(scope: &str, tally: Tally) {
    if tally.fell_back > 0 {
        info!(
            "{scope}: {} git facts read in-process, {} fell back to the git CLI",
            tally.in_process, tally.fell_back
        );
    } else {
        debug!(
            "{scope}: {} git facts read in-process, none fell back",
            tally.in_process
        );
    }
}

/// [`report`] for a pass that repeats faster than its INFO line should: a
/// fallback is logged at INFO at most once per `min_gap_secs` (`last` holds
/// the epoch second of the previous INFO line), and at DEBUG otherwise.
pub(crate) fn report_throttled(
    scope: &str,
    tally: Tally,
    last: &std::sync::atomic::AtomicU64,
    min_gap_secs: u64,
) {
    use std::sync::atomic::Ordering;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let prev = last.load(Ordering::Acquire);
    let due = prev == 0 || now.saturating_sub(prev) >= min_gap_secs;
    if tally.fell_back > 0 && due {
        last.store(now.max(1), Ordering::Release);
        report(scope, tally);
    } else {
        debug!(
            "{scope}: {} git facts read in-process, {} fell back to the git CLI",
            tally.in_process, tally.fell_back
        );
    }
}

/// Fixture repositories for the parity tests here and the publisher's
/// end-to-end capture parity test (`fleet`).
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A hermetic `git -C <dir> <args>` that must succeed; returns stdout.
    pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        cmd.args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "protocol.file.allow=always",
        ])
        .arg("-C")
        .arg(dir)
        .args(args);
        qontinui_runner_lib::git_posture::scrub_repo_local_git_env(&mut cmd);
        let out = cmd.output().expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Write `file`, commit it with message `msg`; returns the new HEAD.
    pub(crate) fn commit(dir: &Path, file: &str, msg: &str) -> String {
        std::fs::write(dir.join(file), msg.as_bytes()).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A non-bare "remote" whose `trunk` holds c1, a merged side branch (so
    /// counts cross a merge commit) and c4.
    pub(crate) fn remote(root: &Path, trunk: &str) -> PathBuf {
        let r = root.join("remote");
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", trunk]);
        commit(&r, "a.txt", "c1");
        git(&r, &["checkout", "-q", "-b", "side"]);
        commit(&r, "side.txt", "s1");
        git(&r, &["checkout", "-q", trunk]);
        commit(&r, "b.txt", "c2");
        git(&r, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        commit(&r, "c.txt", "c4");
        r
    }

    /// `remote` cloned to `<root>/clone`, then a `feature` branch off c2
    /// carrying two commits of its own — two ahead of `origin/<trunk>` and
    /// three behind it (c4, the merge, and the side commit it merged).
    pub(crate) fn clone_on_feature(root: &Path, trunk: &str) -> PathBuf {
        let r = remote(root, trunk);
        let c = root.join("clone");
        git(
            root,
            &["clone", "-q", r.to_str().unwrap(), c.to_str().unwrap()],
        );
        git(&c, &["checkout", "-q", "-b", "feature", "HEAD~2"]);
        commit(&c, "f1.txt", "f1");
        commit(&c, "f2.txt", "f2");
        c
    }
}

#[cfg(test)]
mod tests {
    //! Parity first: each fact through git2 and through the CLI call it
    //! replaces, on the same fixture, must agree exactly.

    use super::fixtures::{clone_on_feature, commit, git};
    use super::*;

    /// The production CLI call: trimmed stdout on exit 0, else `None` — the
    /// shape of the census's and publisher's own `git_capture`.
    fn cli(dir: &Path, args: &[&str]) -> Option<String> {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("-C").arg(dir).args(args);
        let out = cmd.output().expect("git runs");
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Every fact the census and the publisher read, through ONE path.
    #[derive(Debug, PartialEq, Eq)]
    struct Facts {
        branch: Option<String>,
        head: Option<String>,
        commit: Option<(Option<i64>, String)>,
        commit_instant: Option<i64>,
        trunk: Option<String>,
        trunk_oid: Option<String>,
        verify_feature: Option<String>,
        verify_missing: Option<String>,
        head_in_trunk: Option<bool>,
        trunk_in_head: Option<bool>,
        head_in_head: Option<bool>,
        oid_in_trunk: Option<bool>,
        left_right: Option<String>,
        behind: Option<String>,
        ahead: Option<String>,
        to_missing: Option<String>,
    }

    /// The census's own split of `log -1 --format=%ct%n%B`.
    fn split_log(s: &str) -> (Option<i64>, String) {
        crate::agent_worktree::census::split_head_log(s)
    }

    fn cli_ancestor(dir: &Path, a: &str, b: &str) -> Option<bool> {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("-C")
            .arg(dir)
            .args(["merge-base", "--is-ancestor", a, b]);
        match cmd.status().expect("git runs").code() {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        }
    }

    fn facts_cli(dir: &Path) -> Facts {
        let trunk = crate::git_trunk::resolve_trunk_ref_cli(dir);
        let t = trunk.clone().unwrap_or_else(|| "origin/main".to_string());
        let head = cli(dir, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty());
        let h = head.clone().unwrap_or_default();
        Facts {
            branch: cli(dir, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty()),
            commit: cli(dir, &["log", "-1", "--format=%ct%n%B"]).map(|s| split_log(&s)),
            commit_instant: cli(dir, &["log", "-1", "--format=%cI"]).and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|d| d.timestamp())
            }),
            trunk_oid: cli(dir, &["rev-parse", "--verify", "--quiet", &t]),
            verify_feature: cli(dir, &["rev-parse", "--verify", "--quiet", "feature"]),
            verify_missing: cli(dir, &["rev-parse", "--verify", "--quiet", "origin/gone"]),
            head_in_trunk: cli_ancestor(dir, "HEAD", &t),
            trunk_in_head: cli_ancestor(dir, &t, "HEAD"),
            head_in_head: cli_ancestor(dir, "HEAD", "HEAD"),
            oid_in_trunk: cli_ancestor(dir, &h, &t),
            left_right: cli(
                dir,
                &[
                    "rev-list",
                    "--count",
                    "--left-right",
                    &format!("{t}...HEAD"),
                ],
            ),
            behind: cli(dir, &["rev-list", "--count", &format!("HEAD..{t}")]),
            ahead: cli(dir, &["rev-list", "--count", &format!("{t}..HEAD")]),
            to_missing: cli(dir, &["rev-list", "--count", "HEAD..origin/gone"]),
            trunk,
            head,
        }
    }

    /// The same facts through git2; a fact git2 declines reads `Err`, which
    /// the parity check reports as a mismatch rather than hiding.
    fn facts_git2(dir: &Path) -> Result<Facts, String> {
        let repo = open(dir).map_err(|e| format!("open: {e}"))?;
        let e = |what: &'static str| move |err: git2::Error| format!("{what}: {err}");
        let trunk = resolve_trunk_ref(&repo).map_err(e("trunk"))?;
        let t = trunk.clone().unwrap_or_else(|| "origin/main".to_string());
        let head = head_oid(&repo).map_err(e("head"))?;
        let h = head.clone().unwrap_or_default();
        let anc = |a: &str, b: &str| -> Result<Option<bool>, String> {
            // The CLI's "could not ask" (exit 128) is git2's `Err` here.
            match (resolve_commit(&repo, a), resolve_commit(&repo, b)) {
                (Ok(Some(_)), Ok(Some(_))) => is_ancestor(&repo, a, b)
                    .map(Some)
                    .map_err(|err| format!("is_ancestor({a}, {b}): {err}")),
                _ => Ok(None),
            }
        };
        let count = |a: &str, b: &str| -> Result<Option<String>, String> {
            ahead_behind(&repo, a, b)
                .map(|o| o.map(|(_, right)| right.to_string()))
                .map_err(|err| format!("ahead_behind({a}, {b}): {err}"))
        };
        let commit = head_commit(&repo).map_err(e("commit"))?;
        Ok(Facts {
            branch: head_branch(&repo).map_err(e("branch"))?,
            commit: commit.clone().map(|c| (Some(c.committed_epoch), c.body)),
            commit_instant: commit.map(|c| c.committed_epoch),
            trunk_oid: verify_ref(&repo, &t).map_err(e("trunk oid"))?,
            verify_feature: verify_ref(&repo, "feature").map_err(e("verify feature"))?,
            verify_missing: verify_ref(&repo, "origin/gone").map_err(e("verify missing"))?,
            head_in_trunk: anc("HEAD", &t)?,
            trunk_in_head: anc(&t, "HEAD")?,
            head_in_head: anc("HEAD", "HEAD")?,
            oid_in_trunk: if h.is_empty() { Ok(None) } else { anc(&h, &t) }?,
            left_right: ahead_behind(&repo, &t, "HEAD")
                .map_err(e("left_right"))?
                .map(|(l, r)| format!("{l}\t{r}")),
            behind: count("HEAD", &t)?,
            ahead: count(&t, "HEAD")?,
            to_missing: count("HEAD", "origin/gone")?,
            trunk,
            head,
        })
    }

    /// The parity assertion every fixture runs, plus a premise check that the
    /// fixture actually exercised the commit walk (no vacuous all-`None`).
    fn assert_parity(dir: &Path) -> Facts {
        let via_cli = facts_cli(dir);
        let via_git2 = facts_git2(dir).unwrap_or_else(|e| panic!("git2 declined: {e}"));
        assert_eq!(via_git2, via_cli, "git2 and the CLI disagree");
        via_cli
    }

    #[test]
    fn parity_on_a_normal_clone() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        let f = assert_parity(&c);
        assert_eq!(f.branch.as_deref(), Some("feature"));
        assert_eq!(f.trunk.as_deref(), Some("origin/main"));
        assert_eq!(f.left_right.as_deref(), Some("3\t2"));
        assert_eq!(f.behind.as_deref(), Some("3"));
        assert_eq!(f.ahead.as_deref(), Some("2"));
        assert_eq!(f.head_in_trunk, Some(false));
        assert_eq!(f.head_in_head, Some(true));
        assert_eq!(f.verify_missing, None);
        assert_eq!(f.to_missing, None);
        assert_eq!(f.commit.as_ref().map(|c| c.1.as_str()), Some("f2"));
    }

    #[test]
    fn parity_on_a_linked_worktree() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        let wt = t.path().join("wt");
        git(
            &c,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt",
                wt.to_str().unwrap(),
                "origin/main~1",
            ],
        );
        commit(&wt, "w.txt", "w1");
        let f = assert_parity(&wt);
        assert_eq!(f.branch.as_deref(), Some("wt"));
        assert_eq!(f.left_right.as_deref(), Some("1\t1"));
        // The worktree's HEAD, not the primary's.
        assert_eq!(f.commit.as_ref().map(|c| c.1.as_str()), Some("w1"));
    }

    #[test]
    fn parity_on_a_detached_head() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(&c, &["checkout", "-q", "--detach", "origin/main~1"]);
        let f = assert_parity(&c);
        assert_eq!(f.branch, None, "detached: no branch");
        assert!(f.head.is_some());
        assert_eq!(f.head_in_trunk, Some(true));
        assert_eq!(f.trunk_in_head, Some(false));
        assert_eq!(f.left_right.as_deref(), Some("1\t0"));
    }

    #[test]
    fn parity_with_packed_refs_only() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(&c, &["pack-refs", "--all", "--prune"]);
        // Premise: no loose branch or remote-tracking ref is left to read.
        for ns in ["refs/heads", "refs/remotes/origin"] {
            let loose: Vec<_> = std::fs::read_dir(c.join(".git").join(ns))
                .map(|d| {
                    d.flatten()
                        .filter(|e| e.path().is_file() && e.file_name() != "HEAD")
                        .collect()
                })
                .unwrap_or_default();
            assert!(loose.is_empty(), "loose refs remain in {ns}: {loose:?}");
        }
        let f = assert_parity(&c);
        assert_eq!(f.branch.as_deref(), Some("feature"));
        assert!(f.verify_feature.is_some());
        assert_eq!(f.left_right.as_deref(), Some("3\t2"));
    }

    #[test]
    fn parity_on_a_master_trunk() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "master");
        let f = assert_parity(&c);
        assert_eq!(f.trunk.as_deref(), Some("origin/master"));
        assert_eq!(f.left_right.as_deref(), Some("3\t2"));
    }

    #[test]
    fn parity_without_origin_head() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(&c, &["remote", "set-head", "origin", "-d"]);
        let f = assert_parity(&c);
        assert_eq!(f.trunk.as_deref(), Some("origin/main"), "rung 2");

        // A `master` trunk with no `origin/HEAD` has no rung that names it.
        let t2 = tempfile::tempdir().unwrap();
        let m = clone_on_feature(t2.path(), "master");
        git(&m, &["remote", "set-head", "origin", "-d"]);
        let f = assert_parity(&m);
        assert_eq!(f.trunk, None, "honest unknown, never a guessed master");
    }

    #[test]
    fn parity_with_a_stale_origin_head() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(
            &c,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/gone",
            ],
        );
        let f = assert_parity(&c);
        assert_eq!(f.trunk.as_deref(), Some("origin/main"), "falls through");
    }

    #[test]
    fn parity_with_an_empty_commit_message() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(
            &c,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "--allow-empty-message",
                "-m",
                "",
            ],
        );
        let f = assert_parity(&c);
        assert_eq!(f.commit.as_ref().map(|c| c.1.as_str()), Some(""));
        assert!(f.commit.as_ref().and_then(|c| c.0).is_some());
    }

    #[test]
    fn parity_with_a_multi_paragraph_body_and_session_trailer() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(
            &c,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "perf: subject",
                "-m",
                "First paragraph,\nwrapped.",
                "-m",
                "Session-Id: 0a1b2c3d-0000-4000-8000-000000000001\nSession-Name: probe",
            ],
        );
        let f = assert_parity(&c);
        let body = f.commit.map(|c| c.1).unwrap();
        assert_eq!(
            body,
            "perf: subject\n\nFirst paragraph,\nwrapped.\n\n\
             Session-Id: 0a1b2c3d-0000-4000-8000-000000000001\nSession-Name: probe"
        );
        assert_eq!(
            crate::agent_worktree::custody::parse_session_trailers(&body).0,
            Some("0a1b2c3d-0000-4000-8000-000000000001".to_string())
        );
    }

    #[test]
    fn parity_on_an_unborn_branch() {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("fresh");
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        let f = assert_parity(&r);
        assert_eq!(f.branch.as_deref(), Some("main"));
        assert_eq!(f.head, None);
        assert_eq!(f.commit, None);
    }

    /// A branch with a same-named tag: `symbolic-ref --short` prints
    /// `heads/<name>`, which git2 does not reproduce, so it declines — and the
    /// fact, through [`or_cli`], is still the CLI's.
    #[test]
    fn an_ambiguous_branch_name_falls_back_to_the_cli() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        git(&c, &["tag", "feature"]);
        let repo = open(&c).unwrap();
        assert!(head_branch(&repo).is_err(), "git2 must decline");
        let mut tally = Tally::default();
        let got = or_cli(Some(&repo), &mut tally, head_branch, || {
            cli(&c, &["symbolic-ref", "--short", "HEAD"])
        });
        assert_eq!(got.as_deref(), Some("heads/feature"));
        assert_eq!(
            tally,
            Tally {
                in_process: 0,
                fell_back: 1
            }
        );

        // Same for a trunk whose short name a local branch shadows.
        git(&c, &["branch", "origin/main", "HEAD"]);
        assert!(resolve_trunk_ref(&repo).is_err());
        assert_eq!(
            crate::git_trunk::resolve_trunk_ref(&c).as_deref(),
            crate::git_trunk::resolve_trunk_ref_cli(&c).as_deref(),
            "the public resolver still answers what the CLI answers"
        );
    }

    /// Replaced history is read by the CLI and not by libgit2, so commit reads
    /// decline rather than answer from the unreplaced commit.
    #[test]
    fn replaced_history_falls_back_to_the_cli() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        let head = git(&c, &["rev-parse", "HEAD"]);
        let other = git(&c, &["rev-parse", "HEAD~1"]);
        git(&c, &["replace", &head, &other]);
        let repo = open(&c).unwrap();
        assert!(head_commit(&repo).is_err());
        assert!(is_ancestor(&repo, "HEAD", "origin/main").is_err());
        assert!(ahead_behind(&repo, "origin/main", "HEAD").is_err());
        // Ref reads are unaffected by replacement and still answer.
        assert!(head_oid(&repo).is_ok());
    }

    #[test]
    fn a_directory_inside_a_repo_does_not_open() {
        // The CLI would discover the parent; `open` must not, so the caller
        // falls back to the CLI and answers for the parent as before.
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        let sub = c.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(open(&sub).is_err());
    }

    #[test]
    fn or_cli_counts_both_paths() {
        let t = tempfile::tempdir().unwrap();
        let c = clone_on_feature(t.path(), "main");
        let repo = open(&c).unwrap();
        let mut tally = Tally::default();
        let mut cli_calls = 0;
        let a = or_cli(Some(&repo), &mut tally, head_oid, || {
            cli_calls += 1;
            None
        });
        assert!(a.is_some());
        let b: Option<String> = or_cli(None, &mut tally, head_oid, || {
            cli_calls += 1;
            Some("cli".to_string())
        });
        assert_eq!(b.as_deref(), Some("cli"));
        assert_eq!(cli_calls, 1);
        assert_eq!(
            tally,
            Tally {
                in_process: 1,
                fell_back: 1
            }
        );
    }
}
