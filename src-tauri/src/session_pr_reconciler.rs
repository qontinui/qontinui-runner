//! Runner-local per-session PR attribution + status reconciler.
//!
//! Feeds `project.session_prs`, which the Terminal zone-header dropdown reads
//! (`commands::session_prs::session_prs_get`). Ground truth for attribution:
//! every commit a session makes carries a `Session-Id: <claude_session_id>`
//! git trailer (installed machine-wide via a `prepare-commit-msg` hook). So
//! "which PRs did session S open" = PRs whose head-branch HEAD commit carries
//! `Session-Id: S`.
//!
//! Each tick (~30s, plus a best-effort pass at startup):
//!
//! 1. Enumerate open terminal-session records ([`SessionLifecycleStore::open_records`]) —
//!    each carries the `claude_session_id` and the session's `working_dir`.
//! 2. Resolve each session's repo (git toplevel of its cwd) → `owner/name`
//!    from the `origin` remote.
//! 3. In that repo, read every local branch's HEAD-commit `Session-Id`
//!    trailer(s) in ONE `git for-each-ref` and keep the branches whose trailer
//!    names this session.
//! 4. Resolve each matching branch → its PR(s) via the GitHub API
//!    (`GitHubClient::list_prs_for_head`, `state=all`).
//! 5. VERIFY the PR's head-commit `Session-Id` trailer == this session (a
//!    branch touched by multiple sessions carries only the LAST session's
//!    trailer on its HEAD; this disambiguates), then upsert the PR — the
//!    upsert also lands fresh open/merged status.
//! 6. STATUS (Phase 3): for any already-stored PR of this session whose local
//!    branch is gone (so step 4 didn't refresh it this tick), refresh
//!    open/merged via `GitHubClient::get_pr` and `update_session_pr_status`.
//!    Folded into this same tick — NOT a second GitHub poll loop; scoped to
//!    the session's OWN PRs (never a fleet scan).
//!
//! Best-effort throughout: PG unavailable, no GitHub token, a git/gh failure,
//! or an API error skips the affected unit and logs — a passive dropdown never
//! justifies noisy failures.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::process::Command;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::database::pg::PgDb;
use crate::session::session_lifecycle_store::SessionLifecycleStore;
use crate::trigger_system::github_api::GitHubClient;

/// Reconcile cadence. Matches the "similar poller" cadence in the crate
/// (min-10s pollers, 45s lifecycle poll) — 30s is responsive enough for a
/// passive status indicator without hammering the GitHub API.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// One local branch with its HEAD sha and the `Session-Id` trailer value(s) on
/// that HEAD commit.
struct BranchTrailer {
    branch: String,
    sha: String,
    session_ids: Vec<String>,
}

/// Cached per-repo resolution for one tick.
struct RepoInfo {
    owner: String,
    repo: String,
    branches: Vec<BranchTrailer>,
}

/// Start the reconciler as a detached background task for the process
/// lifetime (matching the lifecycle liveness-poll idiom in `main.rs`). Runs a
/// best-effort pass immediately, then every [`POLL_INTERVAL`].
pub fn start(lifecycle_store: std::sync::Arc<SessionLifecycleStore>) {
    tauri::async_runtime::spawn(async move {
        info!(
            "session-PR reconciler started (interval: {}s)",
            POLL_INTERVAL.as_secs()
        );
        loop {
            if let Err(e) = run_tick(&lifecycle_store).await {
                debug!("session-PR reconciler tick error (continuing): {e}");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

/// One reconcile pass. Returns `Err` only for a whole-tick precondition miss
/// (no DB / no token); per-session/per-repo failures are logged and skipped.
async fn run_tick(store: &SessionLifecycleStore) -> Result<(), String> {
    if !crate::database::pg::pg_available() {
        return Err("PG unavailable".to_string());
    }
    let Some(pg_db) = PgDb::try_global() else {
        return Err("PgDb global not set".to_string());
    };

    // Consider only live Claude sessions with a known cwd — the trailer is
    // keyed by `claude_session_id`, so non-Claude providers never match.
    let records: Vec<(Uuid, String)> = store
        .open_records()
        .into_iter()
        .filter(|r| r.provider == "claude")
        .filter_map(|r| {
            let wd = r.working_dir.clone()?;
            let id = Uuid::parse_str(r.claude_session_id.trim()).ok()?;
            Some((id, wd))
        })
        .collect();
    if records.is_empty() {
        return Ok(());
    }

    let Some(token) = resolve_github_token().await else {
        return Err("no GitHub token (env GITHUB_TOKEN/GH_TOKEN or `gh auth token`)".to_string());
    };
    let client = GitHubClient::new(&token)?;

    // Per-repo (git toplevel) resolution cached across sessions that share a
    // checkout: one `remote get-url` + one `for-each-ref` per repo per tick.
    let mut repo_cache: HashMap<String, Option<RepoInfo>> = HashMap::new();

    for (session_id, working_dir) in records {
        let Some(toplevel) = git_toplevel(&working_dir).await else {
            debug!("session-PR reconciler: {working_dir} is not a git repo — skipping");
            continue;
        };

        if !repo_cache.contains_key(&toplevel) {
            let info = resolve_repo(&toplevel).await;
            repo_cache.insert(toplevel.clone(), info);
        }
        let Some(info) = repo_cache.get(&toplevel).and_then(|o| o.as_ref()) else {
            continue;
        };

        if let Err(e) = reconcile_session(&pg_db, &client, session_id, &toplevel, info).await {
            debug!(
                "session-PR reconciler: session {session_id} in {toplevel} failed (skipping): {e}"
            );
        }
    }

    Ok(())
}

/// Attribute + status-refresh one session's PRs within its repo.
async fn reconcile_session(
    pg_db: &PgDb,
    client: &GitHubClient,
    session_id: Uuid,
    repo_dir: &str,
    info: &RepoInfo,
) -> Result<(), String> {
    let session_str = session_id.to_string();
    let repo_full = format!("{}/{}", info.owner, info.repo);

    // (repo_full, pr_number) refreshed via the branch path this tick, so the
    // Phase-3 status pass can skip a redundant get_pr for them.
    let mut refreshed: HashSet<(String, i64)> = HashSet::new();

    // ---- Phase 2: attribution (branch → PR), for THIS session's branches ---
    for bt in info
        .branches
        .iter()
        .filter(|b| b.session_ids.iter().any(|s| s == &session_str))
    {
        let prs = match client
            .list_prs_for_head(&info.owner, &info.repo, &bt.branch)
            .await
        {
            Ok(prs) => prs,
            Err(e) => {
                debug!(
                    "session-PR reconciler: list_prs_for_head({repo_full}, {}) failed: {e}",
                    bt.branch
                );
                continue;
            }
        };

        for pr in prs {
            // VERIFY the PR's head-commit trailer names this session. When the
            // PR head equals the local branch HEAD we already matched, it is
            // verified by construction; otherwise read the PR head's trailer
            // locally (works because the runner pushed it), and fall back to
            // the branch-HEAD match if that object isn't present locally.
            let verified = if pr.head_sha.is_empty() || pr.head_sha == bt.sha {
                true
            } else {
                match read_session_trailers(repo_dir, &pr.head_sha).await {
                    Some(ids) => ids.iter().any(|s| s == &session_str),
                    None => true,
                }
            };
            if !verified {
                continue;
            }

            let pr_number = pr.number as i64;
            let state = pr_state_label(pr.merged, &pr.state);
            let merged_at = pr.merged_at.as_deref().and_then(parse_ts);

            if let Err(e) = pg_db
                .upsert_session_pr(
                    session_id,
                    &repo_full,
                    pr_number,
                    Some(bt.branch.as_str()),
                    Some(state),
                    pr.merged,
                    merged_at,
                )
                .await
            {
                warn!("session-PR reconciler: upsert_session_pr failed: {e}");
                continue;
            }
            refreshed.insert((repo_full.clone(), pr_number));
        }
    }

    // ---- Phase 3: status refresh for stored rows with no live branch -------
    // A session's merged PR often has its local branch deleted, so the branch
    // path above no longer refreshes it. Pull each such stored row's current
    // state directly. Scoped to this session's OWN PRs.
    let stored = pg_db.list_session_prs(session_id).await.unwrap_or_default();
    for row in stored {
        if refreshed.contains(&(row.repo.clone(), row.pr_number)) {
            continue;
        }
        let Some((owner, repo)) = row.repo.split_once('/') else {
            continue;
        };
        match client.get_pr(owner, repo, row.pr_number as u64).await {
            Ok(pr) => {
                let state = pr_state_label(pr.merged, &pr.state);
                // `get_pr`'s PrStatus exposes no merged_at; preserve any stamp
                // the branch path already recorded (frontend renders green off
                // the `merged` bool, so the timestamp is cosmetic).
                if let Err(e) = pg_db
                    .update_session_pr_status(
                        session_id,
                        &row.repo,
                        row.pr_number,
                        Some(state),
                        pr.merged,
                        row.merged_at,
                    )
                    .await
                {
                    warn!("session-PR reconciler: update_session_pr_status failed: {e}");
                }
            }
            Err(e) => {
                debug!(
                    "session-PR reconciler: get_pr({}/#{}) failed (leaving prior status): {e}",
                    row.repo, row.pr_number
                );
            }
        }
    }

    Ok(())
}

/// Map GitHub PR fields to the projection's `pr_state` label.
fn pr_state_label(merged: bool, state: &str) -> &'static str {
    if merged {
        "merged"
    } else if state == "closed" {
        "closed"
    } else {
        "open"
    }
}

/// Parse a GitHub RFC3339 timestamp to `DateTime<Utc>`.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Resolve `owner/name` + the branch→session-trailer map for a repo toplevel.
async fn resolve_repo(toplevel: &str) -> Option<RepoInfo> {
    let remote = git_remote_url(toplevel).await?;
    let (owner, repo) = parse_owner_repo(&remote)?;
    let branches = branch_trailers(toplevel).await;
    Some(RepoInfo {
        owner,
        repo,
        branches,
    })
}

/// `git -C <dir> rev-parse --show-toplevel` — the repo root, or `None` if the
/// dir isn't inside a git work tree.
async fn git_toplevel(dir: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", dir, "rev-parse", "--show-toplevel"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// `git -C <dir> remote get-url origin`.
async fn git_remote_url(dir: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", dir, "remote", "get-url", "origin"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Parse `owner/name` from a GitHub remote URL — SSH (`git@github.com:o/n.git`),
/// HTTPS (`https://github.com/o/n(.git)`), or `x-access-token@` forms.
fn parse_owner_repo(remote: &str) -> Option<(String, String)> {
    let s = remote.trim();
    // Take everything after the host separator: `:` for scp-like SSH,
    // the path after `github.com/` for URL forms.
    let path = if let Some(idx) = s.find("github.com") {
        let rest = &s[idx + "github.com".len()..];
        rest.trim_start_matches([':', '/'])
    } else {
        return None;
    };
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let owner = parts.next()?.trim();
    let name = parts.next()?.trim();
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// Read every local branch's HEAD sha + `Session-Id` trailer value(s) in one
/// `git for-each-ref`. Line format: `<sha> <branch> <trailer values…>`
/// (git ref names contain no whitespace, so the first two whitespace fields
/// are unambiguous; the remainder is the space-joined trailer value set).
async fn branch_trailers(dir: &str) -> Vec<BranchTrailer> {
    let out = match Command::new("git")
        .args([
            "-C",
            dir,
            "for-each-ref",
            "--format=%(objectname) %(refname:short) %(trailers:key=Session-Id,valueonly,separator=%x20)",
            "refs/heads/",
        ])
        .output()
        .await
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            debug!(
                "session-PR reconciler: for-each-ref in {dir} failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return Vec::new();
        }
        Err(e) => {
            debug!("session-PR reconciler: for-each-ref spawn in {dir} failed: {e}");
            return Vec::new();
        }
    };

    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_branch_trailer_line)
        .collect()
}

/// Parse one `for-each-ref` line into a [`BranchTrailer`]. `None` for a line
/// missing the sha/branch fields.
fn parse_branch_trailer_line(line: &str) -> Option<BranchTrailer> {
    let mut it = line.split_whitespace();
    let sha = it.next()?.to_string();
    let branch = it.next()?.to_string();
    let session_ids: Vec<String> = it.map(|s| s.to_string()).collect();
    Some(BranchTrailer {
        branch,
        sha,
        session_ids,
    })
}

/// Read the `Session-Id` trailer value(s) on a specific commit, or `None` if
/// the object isn't present locally (or git fails).
async fn read_session_trailers(dir: &str, sha: &str) -> Option<Vec<String>> {
    let out = Command::new("git")
        .args([
            "-C",
            dir,
            "log",
            "-1",
            "--format=%(trailers:key=Session-Id,valueonly,separator=%x20)",
            sha,
        ])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(s.split_whitespace().map(|t| t.to_string()).collect())
}

/// Resolve a GitHub token, runner-locally: env `GITHUB_TOKEN` / `GH_TOKEN`
/// first, then `gh auth token` (the operator's authenticated GitHub CLI — the
/// same credential the interactive `gh pr create` sessions this feature serves
/// already use). `None` when no source yields a non-empty token.
async fn resolve_github_token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    let out = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() {
        None
    } else {
        Some(tok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_owner_repo_handles_ssh_https_and_token_forms() {
        for (url, want) in [
            (
                "git@github.com:qontinui/qontinui-runner.git",
                ("qontinui", "qontinui-runner"),
            ),
            (
                "https://github.com/qontinui/qontinui-runner.git",
                ("qontinui", "qontinui-runner"),
            ),
            (
                "https://github.com/qontinui/qontinui-runner",
                ("qontinui", "qontinui-runner"),
            ),
            (
                "https://x-access-token:TOKEN@github.com/o/n.git",
                ("o", "n"),
            ),
            ("ssh://git@github.com/o/n.git", ("o", "n")),
        ] {
            let got = parse_owner_repo(url).unwrap();
            assert_eq!((got.0.as_str(), got.1.as_str()), want, "url={url}");
        }
        // Non-GitHub remotes are ignored.
        assert!(parse_owner_repo("https://gitlab.com/o/n.git").is_none());
        assert!(parse_owner_repo("").is_none());
    }

    #[test]
    fn pr_state_label_prefers_merged_then_closed_then_open() {
        assert_eq!(pr_state_label(true, "closed"), "merged");
        assert_eq!(pr_state_label(true, "open"), "merged");
        assert_eq!(pr_state_label(false, "closed"), "closed");
        assert_eq!(pr_state_label(false, "open"), "open");
    }

    #[test]
    fn parse_branch_trailer_line_splits_sha_branch_and_sessions() {
        // No trailer.
        let bt = parse_branch_trailer_line("abc123 main").unwrap();
        assert_eq!(bt.sha, "abc123");
        assert_eq!(bt.branch, "main");
        assert!(bt.session_ids.is_empty());

        // One Session-Id trailer.
        let bt = parse_branch_trailer_line("deadbeef feat/x 11111111-1111-1111-1111-111111111111")
            .unwrap();
        assert_eq!(bt.branch, "feat/x");
        assert_eq!(bt.session_ids, vec!["11111111-1111-1111-1111-111111111111"]);

        // Two (branch touched under two Session-Id trailers on one commit).
        let bt = parse_branch_trailer_line("sha b/1 aaa bbb").unwrap();
        assert_eq!(bt.session_ids, vec!["aaa", "bbb"]);

        // Malformed (sha only) → no branch field.
        assert!(parse_branch_trailer_line("loneword").is_none());
        assert!(parse_branch_trailer_line("").is_none());
    }

    #[test]
    fn parse_ts_roundtrips_github_rfc3339() {
        let dt = parse_ts("2026-07-13T01:02:03Z").unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-07-13T01:02:03+00:00");
        assert!(parse_ts("not-a-date").is_none());
    }
}
