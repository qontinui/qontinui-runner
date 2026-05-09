//! Pure git plumbing for the per-terminal traffic-light feature (Phase 1A of
//! `plans/terminal-traffic-light-and-conflict-bottleneck-plan.md`).
//!
//! No Tauri / `AppState` deps — everything here shells out to `git` directly
//! so it's unit-testable from a tmpdir-init repo. Phase 1B
//! (`session_commit_state_inner`) and the frontend will consume the
//! `CommitState` / `CommitStateStatus` types via `serde`.
//!
//! Wire used by callers:
//!
//! 1. `bucket_by_repo(touched_files)` — group session-touched paths by
//!    enclosing git toplevel (one bucket per distinct repo).
//! 2. For each `(repo, paths)` bucket: `is_mid_merge(repo)` for the merging
//!    flag, then `dirty_subset_in_repo(repo, &paths)` for the subset of
//!    paths that differ from HEAD.
//! 3. Caller assembles `CommitState`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::process_helpers::no_window;

/// The per-session traffic-light state. Sent to the frontend as JSON.
///
/// IMPORTANT: struct fields stay snake_case (default serde behaviour) to
/// match the TS interface Phase 1B will write. Only the enum gets
/// `rename_all = "lowercase"` so the discriminant strings are stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitState {
    pub status: CommitStateStatus,
    /// Total file count in `session_touched_files` for this session.
    pub touched_count: usize,
    /// Subset of `touched_count` whose porcelain entry is non-empty.
    pub dirty_count: usize,
    /// Distinct git toplevels reached after `bucket_by_repo`. Used for the
    /// UI tooltip ("dirty across N repos: …").
    pub repo_roots: Vec<String>,
    /// Subset of `repo_roots` that are mid-merge (`MERGE_HEAD` and friends).
    pub merging_repos: Vec<String>,
    /// Wall-clock millis at the moment the probe finished. Frontend uses
    /// this to debounce/dedupe.
    pub generated_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommitStateStatus {
    Clean,
    Dirty,
    Empty,
    Merging,
    Unknown,
}

impl CommitState {
    /// Convenience constructor for the "tracker is empty" / "no repo" case.
    pub fn empty() -> Self {
        Self {
            status: CommitStateStatus::Empty,
            touched_count: 0,
            dirty_count: 0,
            repo_roots: Vec::new(),
            merging_repos: Vec::new(),
            generated_at_ms: now_ms(),
        }
    }
}

/// Wall-clock millis since UNIX epoch, saturating to 0 on the (impossible)
/// pre-epoch case so this is panic-free.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Group an arbitrary list of absolute file paths by their enclosing git
/// toplevel. Files outside any git repo are dropped silently.
///
/// Implementation detail: caches the `git rev-parse --show-toplevel` lookup
/// keyed by the *parent directory* of each path, so a session that touched
/// 200 files in two repos does at most ~2 git invocations (plus a small
/// number for distinct subdirectories).
pub fn bucket_by_repo(paths: &[String]) -> HashMap<PathBuf, Vec<String>> {
    let mut buckets: HashMap<PathBuf, Vec<String>> = HashMap::new();
    // Cache: parent dir → Some(toplevel) | None (outside any repo).
    let mut cache: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();

    for raw in paths {
        let p = Path::new(raw);
        // We need *some* directory to anchor `git rev-parse` against. If the
        // path is a file, its parent; if it's a dir or doesn't exist, the
        // path itself or its parent — fall back gracefully.
        let anchor: PathBuf = if p.is_dir() {
            p.to_path_buf()
        } else {
            match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => continue, // No anchor — drop.
            }
        };

        let toplevel = cache
            .entry(anchor.clone())
            .or_insert_with(|| resolve_toplevel(&anchor));

        if let Some(top) = toplevel.clone() {
            buckets.entry(top).or_default().push(raw.clone());
        }
        // else: outside any git repo — drop silently per spec.
    }

    buckets
}

/// `git rev-parse --show-toplevel` against `dir`, returning the canonical
/// toplevel as a `PathBuf` or `None` if the directory is outside any repo
/// (or git isn't available, or the dir doesn't exist).
fn resolve_toplevel(dir: &Path) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    let output = no_window("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

/// `git status --porcelain=v1 -z -- <paths>` against the given repo, parse
/// NUL-separated entries, return the subset of `paths` whose status is
/// non-empty (i.e. dirty).
///
/// Critically: ONLY queries the supplied paths, not the whole tree — this is
/// the cost reason for `bucket_by_repo` collapsing files into per-repo
/// invocations.
///
/// Returns the dirty subset as the *exact* path strings the caller passed in
/// (so identity of the entries against `session_touched_files` rows is
/// preserved). We do this by canonicalising both sides via
/// `Path::file_name`/relative-from-repo and matching, but the simpler and
/// more robust approach is: rerun the porcelain check per-path. To keep the
/// fast path, we issue a single bulk `git status` and then derive dirty-ness
/// from "did this path appear in the porcelain output, by suffix match".
pub fn dirty_subset_in_repo(repo: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut args: Vec<&str> = vec!["status", "--porcelain=v1", "-z", "--"];
    for p in paths {
        args.push(p.as_str());
    }

    let output = no_window("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // Porcelain v1 -z format: each entry is `XY <path>\0`, where the leading
    // 3 chars are status + space. For renamed/copied entries (R/C) there's a
    // second NUL-terminated `<orig-path>` immediately after — we don't care
    // about origins; we only need to know which of *our* `paths` were
    // mentioned.
    let stdout = output.stdout;
    let mut dirty_paths: Vec<String> = Vec::new();

    let mut iter = stdout.split(|&b| b == 0u8);
    while let Some(chunk) = iter.next() {
        if chunk.is_empty() {
            continue;
        }
        // Need at least 3 bytes for "XY ".
        if chunk.len() < 3 {
            continue;
        }
        let status_xy = &chunk[..2];
        let path_bytes = &chunk[3..];
        let path_str = String::from_utf8_lossy(path_bytes).into_owned();

        // For R/C entries the *next* chunk is the origin path — skip it so we
        // don't accidentally read it as a separate status entry.
        if status_xy[0] == b'R'
            || status_xy[0] == b'C'
            || status_xy[1] == b'R'
            || status_xy[1] == b'C'
        {
            iter.next();
        }

        dirty_paths.push(path_str);
    }

    // Match the porcelain-emitted paths back to caller-supplied paths.
    // Porcelain emits paths relative to the repo toplevel with forward
    // slashes. The caller passes absolute (or arbitrary) paths. Match by
    // suffix — a porcelain path P matches caller path C iff C, normalised to
    // forward slashes, ends with P. This is robust against the
    // `D:\foo` vs `D:/foo` Windows divergence.
    let mut out: Vec<String> = Vec::new();
    for caller_path in paths {
        let normalised = caller_path.replace('\\', "/");
        let hit = dirty_paths.iter().any(|p| {
            let p_norm = p.replace('\\', "/");
            normalised.ends_with(&p_norm) || p_norm.ends_with(&normalised)
        });
        if hit {
            out.push(caller_path.clone());
        }
    }

    Ok(out)
}

/// Returns true if `repo` is mid-merge / mid-rebase / mid-cherry-pick / etc.
///
/// Resolves the gitdir via `git rev-parse --git-dir` so this works for linked
/// worktrees, where the gitdir is `<main-repo>/.git/worktrees/<name>/` rather
/// than `<wt>/.git`. Checks the union of:
///
/// - `MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `BISECT_LOG` (files)
/// - `rebase-merge/`, `rebase-apply/` (directories)
///
/// Returns `false` on any git failure (don't mask a probe error as
/// "merging" — the caller treats that as `Unknown` upstream).
pub fn is_mid_merge(repo: &Path) -> bool {
    let gitdir = match resolve_gitdir(repo) {
        Some(d) => d,
        None => return false,
    };

    const FILES: &[&str] = &[
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
    ];
    for f in FILES {
        if gitdir.join(f).is_file() {
            return true;
        }
    }
    const DIRS: &[&str] = &["rebase-merge", "rebase-apply"];
    for d in DIRS {
        if gitdir.join(d).is_dir() {
            return true;
        }
    }
    false
}

/// `git rev-parse --git-dir` against `repo` → absolute gitdir path.
///
/// `--git-dir` returns either an absolute path (linked worktrees) or a path
/// relative to the cwd we passed in. We resolve relative to `repo` so the
/// caller doesn't need to care.
fn resolve_gitdir(repo: &Path) -> Option<PathBuf> {
    let output = no_window("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(repo)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let p = PathBuf::from(&s);
    let resolved = if p.is_absolute() { p } else { repo.join(p) };
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// `git init` + identity config. Returns the repo path (the tempdir
    /// itself).
    fn init_repo(td: &TempDir) -> PathBuf {
        let repo = td.path().to_path_buf();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "ci@example.com"]);
        run(&repo, &["config", "user.name", "CI"]);
        run(&repo, &["config", "commit.gpgsign", "false"]);
        repo
    }

    fn run(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap_or_else(|e| panic!("git {:?} failed to spawn: {}", args, e));
        if !output.status.success() {
            panic!(
                "git {:?} exited non-zero: stdout={} stderr={}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn write_file(repo: &Path, rel: &str, contents: &str) -> PathBuf {
        let abs = repo.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs, contents).unwrap();
        abs
    }

    fn commit_all(repo: &Path, msg: &str) {
        run(repo, &["add", "-A"]);
        run(repo, &["commit", "-q", "-m", msg]);
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    // ------------------------------------------------------------------
    // bucket_by_repo
    // ------------------------------------------------------------------

    #[test]
    fn bucket_by_repo_empty_input_yields_empty_map() {
        let out = bucket_by_repo(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn bucket_by_repo_path_outside_any_repo_is_dropped() {
        // Use a tempdir that is NOT a git repo.
        let td = TempDir::new().unwrap();
        let f = write_file(td.path(), "stray.txt", "x");

        let out = bucket_by_repo(&[s(&f)]);
        assert!(out.is_empty(), "non-repo path should be dropped silently");
    }

    #[test]
    fn bucket_by_repo_groups_two_distinct_repos_into_two_buckets() {
        let td_a = TempDir::new().unwrap();
        let td_b = TempDir::new().unwrap();
        let repo_a = init_repo(&td_a);
        let repo_b = init_repo(&td_b);

        let f_a1 = write_file(&repo_a, "a1.txt", "a1");
        let f_a2 = write_file(&repo_a, "sub/a2.txt", "a2");
        commit_all(&repo_a, "init a");
        let f_b1 = write_file(&repo_b, "b1.txt", "b1");
        commit_all(&repo_b, "init b");

        let inputs = vec![s(&f_a1), s(&f_a2), s(&f_b1)];
        let out = bucket_by_repo(&inputs);

        assert_eq!(out.len(), 2, "expected exactly 2 buckets, got {:?}", out);

        // Each bucket key should canonicalise to one of the repo paths.
        // We don't assert exact path equality because git rev-parse may
        // resolve symlinks (macOS /private/var prefix) — match by file
        // count instead.
        let counts: Vec<usize> = out.values().map(|v| v.len()).collect();
        let mut sorted = counts.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2], "expected bucket sizes [1, 2]");
    }

    // ------------------------------------------------------------------
    // dirty_subset_in_repo
    // ------------------------------------------------------------------

    #[test]
    fn dirty_subset_all_clean_returns_empty() {
        let td = TempDir::new().unwrap();
        let repo = init_repo(&td);
        let f1 = write_file(&repo, "f1.txt", "one");
        let f2 = write_file(&repo, "f2.txt", "two");
        commit_all(&repo, "seed");

        let inputs = vec![s(&f1), s(&f2)];
        let dirty = dirty_subset_in_repo(&repo, &inputs).expect("dirty_subset");
        assert!(
            dirty.is_empty(),
            "all clean → empty subset, got {:?}",
            dirty
        );
    }

    #[test]
    fn dirty_subset_mixed_staged_and_unstaged_returns_both() {
        let td = TempDir::new().unwrap();
        let repo = init_repo(&td);
        let f_staged = write_file(&repo, "staged.txt", "v1");
        let f_unstaged = write_file(&repo, "unstaged.txt", "v1");
        let f_clean = write_file(&repo, "clean.txt", "v1");
        commit_all(&repo, "seed");

        // Staged change: modify + add.
        fs::write(&f_staged, "v2-staged").unwrap();
        run(&repo, &["add", "staged.txt"]);

        // Unstaged change: modify only.
        fs::write(&f_unstaged, "v2-unstaged").unwrap();

        // Clean file untouched.

        let inputs = vec![s(&f_staged), s(&f_unstaged), s(&f_clean)];
        let dirty = dirty_subset_in_repo(&repo, &inputs).expect("dirty_subset");

        assert_eq!(
            dirty.len(),
            2,
            "expected exactly 2 dirty entries (staged + unstaged), got {:?}",
            dirty
        );
        assert!(
            dirty.iter().any(|p| p.ends_with("staged.txt")),
            "expected staged.txt in {:?}",
            dirty
        );
        assert!(
            dirty.iter().any(|p| p.ends_with("unstaged.txt")),
            "expected unstaged.txt in {:?}",
            dirty
        );
        assert!(
            !dirty.iter().any(|p| p.ends_with("clean.txt")),
            "expected clean.txt NOT in {:?}",
            dirty
        );
    }

    #[test]
    fn dirty_subset_empty_paths_returns_empty_no_git_call() {
        let td = TempDir::new().unwrap();
        let repo = init_repo(&td);
        write_file(&repo, "f.txt", "hi");
        commit_all(&repo, "seed");

        let dirty = dirty_subset_in_repo(&repo, &[]).expect("dirty_subset");
        assert!(dirty.is_empty());
    }

    // ------------------------------------------------------------------
    // is_mid_merge
    // ------------------------------------------------------------------

    #[test]
    fn is_mid_merge_false_on_clean_repo() {
        let td = TempDir::new().unwrap();
        let repo = init_repo(&td);
        write_file(&repo, "f.txt", "hi");
        commit_all(&repo, "seed");

        assert!(!is_mid_merge(&repo));
    }

    #[test]
    fn is_mid_merge_true_with_merge_head_marker() {
        let td = TempDir::new().unwrap();
        let repo = init_repo(&td);
        write_file(&repo, "f.txt", "hi");
        commit_all(&repo, "seed");

        // Synthesise a MERGE_HEAD marker — content is irrelevant for the
        // detection; existence is the signal.
        fs::write(repo.join(".git").join("MERGE_HEAD"), "deadbeef\n").unwrap();
        assert!(is_mid_merge(&repo));
    }

    #[test]
    fn is_mid_merge_true_with_rebase_merge_dir() {
        let td = TempDir::new().unwrap();
        let repo = init_repo(&td);
        write_file(&repo, "f.txt", "hi");
        commit_all(&repo, "seed");

        // Synthesise a rebase-merge/ directory — git treats its existence as
        // "rebase in progress", same as `git rebase -i` would.
        fs::create_dir_all(repo.join(".git").join("rebase-merge")).unwrap();
        assert!(is_mid_merge(&repo));
    }

    #[test]
    fn is_mid_merge_true_in_linked_worktree_with_marker_under_worktree_gitdir() {
        let td = TempDir::new().unwrap();
        let main = init_repo(&td);
        write_file(&main, "f.txt", "hi");
        commit_all(&main, "seed");

        // Add a linked worktree pointing at a sibling dir.
        let wt_dir = td.path().join("wt-feature");
        run(
            &main,
            &["worktree", "add", "-b", "feature", wt_dir.to_str().unwrap()],
        );

        // The linked worktree's gitdir lives under the main repo at
        // .git/worktrees/<name>/. Synthesise MERGE_HEAD there.
        let wt_gitdir = main.join(".git").join("worktrees").join("wt-feature");
        assert!(
            wt_gitdir.is_dir(),
            "expected linked-worktree gitdir at {:?}",
            wt_gitdir
        );
        fs::write(wt_gitdir.join("MERGE_HEAD"), "deadbeef\n").unwrap();

        assert!(
            is_mid_merge(&wt_dir),
            "is_mid_merge should follow `git rev-parse --git-dir` to the linked worktree's gitdir"
        );
        // Sanity: the main repo itself (no MERGE_HEAD in main .git) is NOT
        // mid-merge.
        assert!(
            !is_mid_merge(&main),
            "main repo should NOT report mid-merge — its .git/MERGE_HEAD doesn't exist"
        );
    }

    // ------------------------------------------------------------------
    // serde
    // ------------------------------------------------------------------

    #[test]
    fn commit_state_status_serialises_lowercase() {
        // The frontend depends on these exact strings — pin them.
        let cases = [
            (CommitStateStatus::Clean, "\"clean\""),
            (CommitStateStatus::Dirty, "\"dirty\""),
            (CommitStateStatus::Empty, "\"empty\""),
            (CommitStateStatus::Merging, "\"merging\""),
            (CommitStateStatus::Unknown, "\"unknown\""),
        ];
        for (variant, expected) in cases {
            let got = serde_json::to_string(&variant).unwrap();
            assert_eq!(got, expected, "wrong wire string for {:?}", variant);
        }
    }

    #[test]
    fn commit_state_struct_fields_stay_snake_case() {
        let cs = CommitState {
            status: CommitStateStatus::Empty,
            touched_count: 3,
            dirty_count: 1,
            repo_roots: vec!["/a".into()],
            merging_repos: vec![],
            generated_at_ms: 42,
        };
        let v: serde_json::Value = serde_json::to_value(&cs).unwrap();
        // Pin the snake_case keys — frontend TS interface uses these
        // verbatim.
        for key in [
            "status",
            "touched_count",
            "dirty_count",
            "repo_roots",
            "merging_repos",
            "generated_at_ms",
        ] {
            assert!(
                v.get(key).is_some(),
                "expected snake_case key {:?} in {:?}",
                key,
                v
            );
        }
    }
}
