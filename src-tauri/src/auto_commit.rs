//! Per-cwd serialized git commit helper.
//!
//! Phase B of the Commit Progress feature. Provides a single async function,
//! [`commit_files`], that runs `git add <explicit list>` followed by
//! `git commit -m <message>` in a specified working directory.
//!
//! Concurrent calls targeting the same `cwd` are serialized through a per-cwd
//! `tokio::sync::Mutex`, so two callers cannot race on the git index lock.
//! Calls on distinct cwds run in parallel.
//!
//! ## Behavior
//!
//! - Empty file list → `Ok(NothingToCommit)`, no git operations.
//! - Each path is staged with `git add -- <path>`. Paths whose staging fails
//!   (e.g., the file was deleted between Phase A registration and the commit)
//!   are logged at `warn!` and skipped. If *all* paths fail, the helper
//!   returns `Err`.
//! - After staging, `git diff --cached --quiet` is consulted; a zero diff
//!   yields `Ok(NothingToCommit)` rather than producing an empty commit.
//! - The commit runs `git commit -m <message> --no-edit` *with* hooks enabled
//!   (no `--no-verify`). The user's pre-commit hooks run normally; if they
//!   fail, the function returns `Err` and no commit is created.
//! - On success, the new HEAD hash is captured via `git rev-parse HEAD` and
//!   returned as [`CommitOutcome::Committed`].
//!
//! ## Mutex strategy
//!
//! A module-private `Lazy<std::sync::Mutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>>`.
//! Lookup canonicalizes the cwd (falling back to the literal path on failure),
//! grabs the outer std mutex briefly to fetch-or-insert an
//! `Arc<TokioMutex<()>>`, drops the outer guard immediately, then awaits the
//! inner mutex. The inner guard is held across all git subprocess calls and
//! released by RAII when the function returns.
//!
//! The mutex is in-process only — multiple runner processes touching the same
//! cwd would still race. The runner is single-process per machine, so this is
//! acceptable.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex as TokioMutex;
use tracing::{info, warn};

use crate::worktree::run_git_command;

// =============================================================================
// Public API
// =============================================================================

/// Outcome of a [`commit_files`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// A commit was created. `hash` is the new HEAD SHA (full, not abbreviated).
    Committed { hash: String },
    /// No commit was created — either the file list was empty, or the listed
    /// files matched HEAD exactly so staging produced no diff.
    NothingToCommit,
}

/// Stage the specified files and create a commit in `cwd`.
///
/// See module-level docs for full semantics. Concurrent calls on the same
/// `cwd` are serialized; calls on different cwds run in parallel.
pub async fn commit_files(
    cwd: &Path,
    files: &[String],
    message: &str,
) -> Result<CommitOutcome, String> {
    if files.is_empty() {
        info!(
            cwd = %cwd.display(),
            "auto_commit: empty file list, skipping git ops"
        );
        return Ok(CommitOutcome::NothingToCommit);
    }

    let lock = acquire_cwd_lock(cwd);
    let _guard = lock.lock().await;

    // Stage each file individually so one bad path doesn't kill the rest.
    // We track the surviving paths so the eventual `git commit --` only
    // commits those (preserving any unrelated staged work the caller had).
    let mut surviving: Vec<&str> = Vec::with_capacity(files.len());
    let mut failures: Vec<String> = Vec::new();
    for path in files {
        match run_git_command(cwd, &["add", "--", path.as_str()]) {
            Ok(_) => {
                surviving.push(path.as_str());
            }
            Err(e) => {
                warn!(
                    cwd = %cwd.display(),
                    path = %path,
                    error = %e,
                    "auto_commit: git add failed for path, skipping"
                );
                failures.push(format!("{}: {}", path, e));
            }
        }
    }

    if surviving.is_empty() {
        return Err(format!(
            "auto_commit: all {} path(s) failed to stage: {}",
            files.len(),
            failures.join("; ")
        ));
    }

    // Did staging produce a diff against HEAD for *these specific paths*?
    // (We only consider the surviving paths — unrelated staged work in the
    // index is the caller's business and must not influence this decision.)
    if !has_staged_diff_for(cwd, &surviving)? {
        info!(
            cwd = %cwd.display(),
            "auto_commit: listed paths produced no diff vs HEAD, skipping commit"
        );
        return Ok(CommitOutcome::NothingToCommit);
    }

    // Run the commit, restricting to the surviving paths via pathspec so any
    // unrelated staged work in the index is left untouched. Hooks ARE allowed
    // to run (no --no-verify).
    let mut commit_args: Vec<&str> = vec!["commit", "-m", message, "--no-edit", "--"];
    commit_args.extend(surviving.iter().copied());
    run_git_command(cwd, &commit_args).map_err(|e| {
        // Surface the raw git/hook stderr so callers can show it to the user.
        format!("auto_commit: git commit failed: {}", e)
    })?;

    let hash = run_git_command(cwd, &["rev-parse", "HEAD"])
        .map_err(|e| format!("auto_commit: failed to read HEAD after commit: {}", e))?
        .trim()
        .to_string();

    info!(
        cwd = %cwd.display(),
        hash = %hash,
        staged = surviving.len(),
        skipped = failures.len(),
        "auto_commit: commit created"
    );

    Ok(CommitOutcome::Committed { hash })
}

// =============================================================================
// Internals
// =============================================================================

/// Per-cwd serialization. Outer std mutex is held only long enough to
/// fetch-or-insert the inner `Arc<TokioMutex<()>>`; the inner mutex is then
/// awaited outside the outer lock.
static CWD_LOCKS: Lazy<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

fn acquire_cwd_lock(cwd: &Path) -> Arc<TokioMutex<()>> {
    // Canonicalize so `./repo` and `repo` and `/abs/repo` collapse to the same
    // entry. Fall back to the literal path if canonicalize fails (e.g., the
    // path doesn't exist yet — shouldn't happen in normal use, but we don't
    // want to panic).
    let key = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    let mut map = CWD_LOCKS
        .lock()
        .expect("auto_commit CWD_LOCKS mutex poisoned");
    Arc::clone(
        map.entry(key)
            .or_insert_with(|| Arc::new(TokioMutex::new(()))),
    )
}

/// Returns `true` if any of `paths` has a staged change vs HEAD.
///
/// Uses `git diff --cached --quiet -- <paths>`, which exits 0 (no diff) or 1
/// (diff present) for the restricted pathspec. Both are non-error outcomes;
/// only spawn failures or signals are real errors. We invoke `Command`
/// directly here since `run_git_command` folds non-zero exit into `Err`.
fn has_staged_diff_for(cwd: &Path, paths: &[&str]) -> Result<bool, String> {
    let mut args: Vec<&str> = vec!["diff", "--cached", "--quiet", "--"];
    args.extend(paths.iter().copied());

    let output = std::process::Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("auto_commit: failed to spawn git diff --cached: {}", e))?;

    match output.status.code() {
        Some(0) => Ok(false), // no diff
        Some(1) => Ok(true),  // diff present
        Some(other) => Err(format!(
            "auto_commit: git diff --cached --quiet exited {}: {}",
            other,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        None => Err("auto_commit: git diff --cached --quiet killed by signal".into()),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Create a temporary git repo with one initial commit so HEAD is valid.
    /// Returns the TempDir (keep it alive for the duration of the test) and
    /// its canonicalized path.
    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("failed to create tempdir");
        let path = dir
            .path()
            .canonicalize()
            .expect("failed to canonicalize tempdir");

        run(&path, &["init", "-q", "-b", "main"]);
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test User"]);
        run(&path, &["config", "commit.gpgsign", "false"]);

        // Initial commit so HEAD exists.
        fs::write(path.join("README.md"), "init\n").unwrap();
        run(&path, &["add", "README.md"]);
        run(&path, &["commit", "-q", "-m", "init"]);

        (dir, path)
    }

    fn run(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {:?}: {}", args, e));
        assert!(
            out.status.success(),
            "git {:?} in {:?} failed:\nstdout: {}\nstderr: {}",
            args,
            cwd,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn try_run(cwd: &Path, args: &[&str]) -> (bool, String, String) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("failed to spawn git");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn head_hash(cwd: &Path) -> String {
        run(cwd, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn commit_count(cwd: &Path) -> usize {
        run(cwd, &["rev-list", "--count", "HEAD"])
            .trim()
            .parse()
            .unwrap()
    }

    // ------------------------------------------------------------------------
    // Happy path
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn commits_two_files_successfully() {
        let (_dir, repo) = init_repo();
        fs::write(repo.join("a.txt"), "alpha\n").unwrap();
        fs::write(repo.join("b.txt"), "bravo\n").unwrap();

        let outcome = commit_files(
            &repo,
            &["a.txt".into(), "b.txt".into()],
            "feat: add a and b",
        )
        .await
        .expect("commit_files should succeed");

        match outcome {
            CommitOutcome::Committed { hash } => {
                assert_eq!(hash, head_hash(&repo));
                let names = run(&repo, &["show", "--name-only", "--format=", "HEAD"]);
                let listed: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
                assert!(listed.contains(&"a.txt"), "a.txt missing from {:?}", listed);
                assert!(listed.contains(&"b.txt"), "b.txt missing from {:?}", listed);
                assert_eq!(listed.len(), 2, "extra files in commit: {:?}", listed);
            }
            CommitOutcome::NothingToCommit => panic!("expected Committed, got NothingToCommit"),
        }
    }

    // ------------------------------------------------------------------------
    // Empty file list
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn empty_file_list_returns_nothing_to_commit() {
        let (_dir, repo) = init_repo();
        let before = head_hash(&repo);

        let outcome = commit_files(&repo, &[], "should not commit")
            .await
            .expect("empty list should be Ok");

        assert_eq!(outcome, CommitOutcome::NothingToCommit);
        assert_eq!(head_hash(&repo), before, "HEAD must not advance");
    }

    // ------------------------------------------------------------------------
    // Listed files match HEAD → no diff → no commit
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn unchanged_files_return_nothing_to_commit() {
        let (_dir, repo) = init_repo();
        let before = head_hash(&repo);

        // README.md is already in HEAD with content "init\n". List it without
        // modifying it.
        let outcome = commit_files(&repo, &["README.md".into()], "noop")
            .await
            .expect("unchanged file list should be Ok");

        assert_eq!(outcome, CommitOutcome::NothingToCommit);
        assert_eq!(head_hash(&repo), before, "HEAD must not advance");
    }

    // ------------------------------------------------------------------------
    // Dirty index outside file-set is preserved
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn unrelated_staged_changes_are_preserved() {
        let (_dir, repo) = init_repo();

        fs::write(repo.join("a.txt"), "alpha\n").unwrap();
        fs::write(repo.join("b.txt"), "bravo\n").unwrap();

        // Manually stage b.txt without committing it.
        run(&repo, &["add", "b.txt"]);

        // Helper commits only a.txt.
        let outcome = commit_files(&repo, &["a.txt".into()], "only a")
            .await
            .expect("commit should succeed");
        assert!(matches!(outcome, CommitOutcome::Committed { .. }));

        // The new HEAD must contain only a.txt.
        let names = run(&repo, &["show", "--name-only", "--format=", "HEAD"]);
        let listed: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(listed, vec!["a.txt"], "commit should only include a.txt");

        // b.txt must still be staged (cached) but uncommitted.
        let staged = run(&repo, &["diff", "--cached", "--name-only"]);
        let staged_names: Vec<&str> = staged.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(
            staged_names,
            vec!["b.txt"],
            "b.txt must remain staged after helper runs"
        );
    }

    // ------------------------------------------------------------------------
    // Concurrent calls on same cwd serialize cleanly
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn concurrent_calls_same_cwd_serialize() {
        let (_dir, repo) = init_repo();
        let before_count = commit_count(&repo);

        fs::write(repo.join("c1.txt"), "one\n").unwrap();
        fs::write(repo.join("c2.txt"), "two\n").unwrap();

        let repo_a = repo.clone();
        let repo_b = repo.clone();

        let h1 =
            tokio::spawn(
                async move { commit_files(&repo_a, &["c1.txt".into()], "concurrent 1").await },
            );
        let h2 =
            tokio::spawn(
                async move { commit_files(&repo_b, &["c2.txt".into()], "concurrent 2").await },
            );

        let (r1, r2) = tokio::join!(h1, h2);
        let o1 = r1.expect("task 1 join").expect("task 1 commit");
        let o2 = r2.expect("task 2 join").expect("task 2 commit");

        assert!(matches!(o1, CommitOutcome::Committed { .. }));
        assert!(matches!(o2, CommitOutcome::Committed { .. }));

        // Two new commits should now be on HEAD's history.
        let after_count = commit_count(&repo);
        assert_eq!(
            after_count,
            before_count + 2,
            "expected 2 new commits, got {} (before {})",
            after_count,
            before_count
        );

        let log = run(&repo, &["log", "--format=%s", "-n", "2"]);
        let subjects: Vec<&str> = log.lines().collect();
        assert!(subjects.contains(&"concurrent 1"));
        assert!(subjects.contains(&"concurrent 2"));
    }

    // ------------------------------------------------------------------------
    // Concurrent calls on distinct cwds both succeed (no deadlock)
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn concurrent_calls_distinct_cwds_both_succeed() {
        let (_dir1, repo1) = init_repo();
        let (_dir2, repo2) = init_repo();

        fs::write(repo1.join("x.txt"), "x\n").unwrap();
        fs::write(repo2.join("y.txt"), "y\n").unwrap();

        let r1 = repo1.clone();
        let r2 = repo2.clone();

        let h1 =
            tokio::spawn(async move { commit_files(&r1, &["x.txt".into()], "repo1 commit").await });
        let h2 =
            tokio::spawn(async move { commit_files(&r2, &["y.txt".into()], "repo2 commit").await });

        let (res1, res2) = tokio::join!(h1, h2);
        let o1 = res1.expect("join 1").expect("commit 1");
        let o2 = res2.expect("join 2").expect("commit 2");

        assert!(matches!(o1, CommitOutcome::Committed { .. }));
        assert!(matches!(o2, CommitOutcome::Committed { .. }));

        assert_eq!(
            run(&repo1, &["log", "--format=%s", "-n", "1"]).trim(),
            "repo1 commit"
        );
        assert_eq!(
            run(&repo2, &["log", "--format=%s", "-n", "1"]).trim(),
            "repo2 commit"
        );
    }

    // ------------------------------------------------------------------------
    // Pre-commit hook failure propagates as Err and no commit lands
    // ------------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn failing_pre_commit_hook_yields_err_and_no_commit() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, repo) = init_repo();
        let before = head_hash(&repo);
        let before_count = commit_count(&repo);

        let hook_path = repo.join(".git").join("hooks").join("pre-commit");
        fs::write(
            &hook_path,
            "#!/bin/sh\necho 'pre-commit refused' >&2\nexit 1\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();

        fs::write(repo.join("hooked.txt"), "x\n").unwrap();

        let result = commit_files(&repo, &["hooked.txt".into()], "should be blocked").await;

        assert!(
            result.is_err(),
            "expected Err from blocked hook, got {:?}",
            result
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("commit failed") || err.contains("pre-commit"),
            "error should mention commit/hook failure: {}",
            err
        );

        assert_eq!(head_hash(&repo), before, "HEAD must not advance");
        assert_eq!(commit_count(&repo), before_count, "no new commits");
    }

    // ------------------------------------------------------------------------
    // All-paths-fail returns Err
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn all_paths_fail_returns_err() {
        let (_dir, repo) = init_repo();
        let before = head_hash(&repo);

        // Path doesn't exist; git add -- <missing> will fail.
        let result =
            commit_files(&repo, &["does-not-exist.txt".into()], "should never commit").await;

        assert!(result.is_err(), "expected Err, got {:?}", result);
        assert_eq!(head_hash(&repo), before, "HEAD must not advance");
    }

    // ------------------------------------------------------------------------
    // Partial failure: one bad path, one good path → commits the good one
    // ------------------------------------------------------------------------

    #[tokio::test]
    async fn partial_failure_commits_good_paths() {
        let (_dir, repo) = init_repo();
        fs::write(repo.join("good.txt"), "good\n").unwrap();

        let outcome = commit_files(
            &repo,
            &["does-not-exist.txt".into(), "good.txt".into()],
            "partial",
        )
        .await
        .expect("partial-failure should still commit the good path");

        assert!(matches!(outcome, CommitOutcome::Committed { .. }));

        let names = run(&repo, &["show", "--name-only", "--format=", "HEAD"]);
        let listed: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(listed, vec!["good.txt"]);

        // Sanity: missing path was not magically created.
        let (ok, _, _) = try_run(&repo, &["cat-file", "-e", "HEAD:does-not-exist.txt"]);
        assert!(!ok, "missing path should not be in tree");
    }
}
