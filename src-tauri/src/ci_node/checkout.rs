//! Candidate checkout: fetch the dispatched SHA into the warm primary
//! checkout, then materialize an ephemeral detached worktree for the build.
//!
//! Layout (all under QONTINUI_ROOT):
//! - primary repo dir: `<root>/<repo basename>` — the fetch target (warm
//!   objects, warm git config).
//! - CI worktree: `<root>/.ci-worktrees/<dispatch_id>` — always removed in
//!   cleanup, success or failure.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

/// Fetch budget — candidate refs are usually a few objects on top of a
/// warm clone, but a cold repo can be slow.
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(600);
/// Everything else (cat-file, worktree add/remove/prune) is local.
const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(120);

/// Run git in `repo_dir`, capturing combined output. Err carries a
/// log-worthy one-liner.
async fn run_git(repo_dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = crate::process_helpers::tokio_no_window("git");
    cmd.current_dir(repo_dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let fut = async {
        let out = cmd
            .output()
            .await
            .map_err(|e| format!("spawn git {}: {e}", args.join(" ")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if out.status.success() {
            Ok(stdout)
        } else {
            Err(format!(
                "git {} failed ({}): {}",
                args.join(" "),
                out.status,
                if stderr.is_empty() { &stdout } else { &stderr }
            ))
        }
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "git {} timed out after {}s",
            args.join(" "),
            timeout.as_secs()
        )),
    }
}

/// The CI worktree path for a dispatch.
pub(crate) fn ci_worktree_path(root: &Path, dispatch_id: &str) -> PathBuf {
    root.join(".ci-worktrees").join(dispatch_id)
}

/// Fetch + verify + worktree-add. Returns the worktree path.
pub(crate) async fn prepare_worktree(
    root: &Path,
    repo: &str,
    dispatch_id: &str,
    fetch_url: &str,
    candidate_ref: &str,
    head_sha: &str,
) -> Result<PathBuf, String> {
    let repo_dir = root.join(crate::agent_runtime::local_repo_name(repo));
    if !repo_dir.join(".git").exists() {
        return Err(format!(
            "primary checkout {} not present on this device (no .git)",
            repo_dir.display()
        ));
    }

    // Warm-SHA precheck (plan Phase 2): the primary checkout's agent daemons
    // fetch constantly, so the dispatched commit is often already local —
    // skip the network round-trip entirely when `git cat-file -e` says so.
    let already_local = run_git(
        &repo_dir,
        &["cat-file", "-e", &format!("{head_sha}^{{commit}}")],
        GIT_LOCAL_TIMEOUT,
    )
    .await
    .is_ok();
    if already_local {
        info!(
            "ci_node: {head_sha} already present in {} — skipping fetch",
            repo_dir.display()
        );
    } else {
        // Fetch the candidate ref; fall back to fetching the bare SHA (works
        // when the server allows uploadpack.allowReachableSHA1InWant or the
        // ref was already pruned but the objects are reachable).
        match run_git(
            &repo_dir,
            &["fetch", fetch_url, candidate_ref],
            GIT_FETCH_TIMEOUT,
        )
        .await
        {
            Ok(_) => {}
            Err(ref_err) => {
                warn!("ci_node: candidate-ref fetch failed ({ref_err}); trying bare-SHA fetch");
                if let Err(sha_err) = run_git(
                    &repo_dir,
                    &["fetch", fetch_url, head_sha],
                    GIT_FETCH_TIMEOUT,
                )
                .await
                {
                    // The cat-file verify below is the real gate, so only
                    // log here.
                    warn!(
                        "ci_node: bare-SHA fetch also failed ({sha_err}); relying on local objects"
                    );
                }
            }
        }
    }

    // Hard gate: the dispatched commit must exist locally now.
    run_git(
        &repo_dir,
        &["cat-file", "-e", &format!("{head_sha}^{{commit}}")],
        GIT_LOCAL_TIMEOUT,
    )
    .await
    .map_err(|e| format!("head_sha {head_sha} not present after fetch from {fetch_url}: {e}"))?;

    let wt_path = ci_worktree_path(root, dispatch_id);
    if wt_path.exists() {
        // Stale leftover from a crashed prior attempt — clear it first.
        warn!(
            "ci_node: stale CI worktree {} exists; removing before re-add",
            wt_path.display()
        );
        cleanup_worktree(root, repo, dispatch_id).await;
    }
    if let Some(parent) = wt_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }

    let wt_str = wt_path.to_string_lossy().to_string();
    run_git(
        &repo_dir,
        &["worktree", "add", "--detach", &wt_str, head_sha],
        GIT_LOCAL_TIMEOUT,
    )
    .await?;
    info!(
        "ci_node: worktree ready at {} for {repo}@{head_sha}",
        wt_path.display()
    );
    Ok(wt_path)
}

/// Always-run cleanup: `git worktree remove --force` + prune, then a
/// best-effort directory delete for anything git left behind. Failure is
/// logged, never propagated (cleanup runs on failure paths too).
pub(crate) async fn cleanup_worktree(root: &Path, repo: &str, dispatch_id: &str) {
    let repo_dir = root.join(crate::agent_runtime::local_repo_name(repo));
    let wt_path = ci_worktree_path(root, dispatch_id);
    let wt_str = wt_path.to_string_lossy().to_string();
    if let Err(e) = run_git(
        &repo_dir,
        &["worktree", "remove", "--force", &wt_str],
        GIT_LOCAL_TIMEOUT,
    )
    .await
    {
        warn!("ci_node: worktree remove failed (continuing to prune): {e}");
    }
    if let Err(e) = run_git(&repo_dir, &["worktree", "prune"], GIT_LOCAL_TIMEOUT).await {
        warn!("ci_node: worktree prune failed: {e}");
    }
    if wt_path.exists() {
        if let Err(e) = std::fs::remove_dir_all(&wt_path) {
            warn!(
                "ci_node: residual worktree dir {} not removable: {e}",
                wt_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_worktree_path_is_dispatch_scoped_under_root() {
        let p = ci_worktree_path(Path::new("/root"), "d-123");
        assert!(p.starts_with("/root"));
        assert!(p.ends_with(PathBuf::from(".ci-worktrees").join("d-123")));
    }
}
