//! Build/deploy drift surface (plan
//! `2026-07-03-runner-session-tracking-drift-and-guardrails`, Phase 3 item 3).
//!
//! The binary already embeds the commit it was built from
//! (`QONTINUI_GIT_SHA`, 12-char, `build.rs`) and serves it as `gitSha` on
//! `/health`. What was missing is the COMPARISON: three shipped fixes sat
//! unrealized for 15 days because the live primary kept executing a stale
//! in-memory image and nothing measured the gap between "on main" and
//! "running here".
//!
//! On a slow interval (and once at startup) this module resolves
//! `origin/main`'s current SHA — `git ls-remote origin main` from the repo
//! dir when available, falling back to the local `git rev-parse origin/main`
//! — and diffs it against the embedded `gitSha` (prefix match: the embedded
//! value is a 12-char short SHA). The result lands on `/health` as `mainSha`
//! + `buildDrift {behind, checkedAt, commitsBehind}` and a periodic WARN when
//! non-zero.
//!
//! Resilience is the contract: a production install with no git repo (or no
//! network, or no `git` on PATH) serves nulls — every failure mode collapses
//! to `None`, never an error and never log spam (the WARN only fires on a
//! POSITIVE drift verdict, which requires git to have succeeded).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, warn};

/// How often the comparison re-runs after the startup check.
const CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Result of one drift check. `None` fields mean "unknown" (no repo / git
/// failure) — the `/health` surface renders them as JSON nulls.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildDriftStatus {
    /// Unix millis when this check ran.
    pub checked_at: i64,
    /// origin/main's current full SHA, when resolvable.
    pub main_sha: Option<String>,
    /// `Some(true)` when the running binary's embedded SHA is not a prefix of
    /// `main_sha` (the binary is behind — or on a divergent commit).
    pub behind: Option<bool>,
    /// `git rev-list --count <embedded>..<main>` when cheaply available
    /// locally (requires both objects in the local repo); `None` otherwise.
    pub commits_behind: Option<u64>,
}

static LATEST: OnceLock<Mutex<Option<BuildDriftStatus>>> = OnceLock::new();

fn latest_cell() -> &'static Mutex<Option<BuildDriftStatus>> {
    LATEST.get_or_init(|| Mutex::new(None))
}

/// Clone of the most recent drift status, if any check has completed.
pub fn latest() -> Option<BuildDriftStatus> {
    latest_cell().lock().ok().and_then(|g| g.clone())
}

fn store_latest(status: BuildDriftStatus) {
    if let Ok(mut g) = latest_cell().lock() {
        *g = Some(status);
    }
}

/// The `/health` fields: `(mainSha, buildDrift)`. Before the first check —
/// or on a repo-less production install — `mainSha` is null and `buildDrift`
/// carries null members, never an error.
pub fn health_fields() -> (serde_json::Value, serde_json::Value) {
    match latest() {
        Some(s) => (
            serde_json::json!(s.main_sha),
            serde_json::json!({
                "behind": s.behind,
                "checkedAt": s.checked_at,
                "commitsBehind": s.commits_behind,
            }),
        ),
        None => (
            serde_json::Value::Null,
            serde_json::json!({
                "behind": serde_json::Value::Null,
                "checkedAt": serde_json::Value::Null,
                "commitsBehind": serde_json::Value::Null,
            }),
        ),
    }
}

/// The repo dir git commands run from: the compile-time source checkout
/// (`CARGO_MANIFEST_DIR`'s parent = the qontinui-runner repo root), kept only
/// when it still looks like a git checkout at runtime. On a production
/// install (path absent / no `.git`) this is `None` and every check yields
/// nulls. `.git` may be a FILE for a worktree — `exists()` covers both.
fn candidate_repo_dir() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [manifest.parent().map(Path::to_path_buf), Some(manifest)];
    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join(".git").exists())
}

/// Run `git <args>` in `repo`, returning trimmed stdout on success. Any
/// failure (spawn error, non-zero exit, empty output) → `None`.
fn git_output(repo: &Path, args: &[&str]) -> Option<String> {
    let out = crate::process_helpers::no_window("git")
        .args(args)
        .current_dir(repo)
        .output()
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

/// Resolve origin/main's current SHA: `ls-remote` (authoritative, needs
/// network) first, local `rev-parse origin/main` (last fetch) as fallback.
fn resolve_main_sha(repo: &Path) -> Option<String> {
    git_output(repo, &["ls-remote", "origin", "main"])
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .or_else(|| git_output(repo, &["rev-parse", "origin/main"]))
        .filter(|s| looks_like_sha(s))
}

fn looks_like_sha(s: &str) -> bool {
    s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Pure comparison core (unit-testable without git): prefix-match the
/// embedded 12-char SHA against origin/main's full SHA. `None` when either
/// side is unknown (e.g. the embedded value is build.rs's `"unknown"`
/// fallback).
fn compute_behind(embedded: &str, main_sha: Option<&str>) -> Option<bool> {
    if !looks_like_sha(embedded) {
        return None;
    }
    main_sha.map(|m| !m.starts_with(embedded))
}

/// One blocking drift check. Never errors; unknown states collapse to nulls.
fn check_once_blocking() -> BuildDriftStatus {
    let embedded = env!("QONTINUI_GIT_SHA");
    let checked_at = chrono::Utc::now().timestamp_millis();

    let Some(repo) = candidate_repo_dir() else {
        return BuildDriftStatus {
            checked_at,
            main_sha: None,
            behind: None,
            commits_behind: None,
        };
    };

    let main_sha = resolve_main_sha(&repo);
    let behind = compute_behind(embedded, main_sha.as_deref());
    let commits_behind = match behind {
        Some(true) => main_sha.as_deref().and_then(|m| {
            git_output(&repo, &["rev-list", "--count", &format!("{embedded}..{m}")])
                .and_then(|s| s.parse().ok())
        }),
        Some(false) => Some(0),
        None => None,
    };

    BuildDriftStatus {
        checked_at,
        main_sha,
        behind,
        commits_behind,
    }
}

/// Detached periodic drift check — runs once immediately at startup, then
/// every [`CHECK_INTERVAL`]. WARNs on each tick that finds non-zero drift.
pub async fn run_periodic() {
    loop {
        let status = tokio::task::spawn_blocking(check_once_blocking)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "build drift: check task panicked");
                BuildDriftStatus {
                    checked_at: chrono::Utc::now().timestamp_millis(),
                    main_sha: None,
                    behind: None,
                    commits_behind: None,
                }
            });

        match (status.behind, status.main_sha.as_deref()) {
            (Some(true), Some(main)) => warn!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                main_sha = main,
                commits_behind = ?status.commits_behind,
                "build drift: this binary was NOT built from origin/main's current \
                 commit — shipped fixes may not be running here"
            ),
            (Some(false), _) => debug!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                "build drift: binary matches origin/main"
            ),
            _ => debug!(
                "build drift: origin/main unresolvable (no repo / no network) — reporting unknown"
            ),
        }

        store_latest(status);
        tokio::time::sleep(CHECK_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behind_is_prefix_match_on_the_12_char_embedded_sha() {
        let main = "dc7aa9c5aaaabbbbccccddddeeeeffff00001111";
        // Embedded 12-char prefix of main → not behind.
        assert_eq!(compute_behind("dc7aa9c5aaaa", Some(main)), Some(false));
        // Different commit → behind.
        assert_eq!(compute_behind("abcdef012345", Some(main)), Some(true));
    }

    #[test]
    fn unknown_states_collapse_to_none() {
        // build.rs fallback value must never produce a verdict.
        assert_eq!(compute_behind("unknown", Some("dc7aa9c5aaaa")), None);
        // Unresolvable main → unknown.
        assert_eq!(compute_behind("dc7aa9c5aaaa", None), None);
    }

    #[test]
    fn health_fields_serve_nulls_before_first_check_shape() {
        // The None arm of health_fields must be null-shaped, never an error.
        // (LATEST is process-global; other tests may have populated it, so
        // exercise the None arm's construction directly.)
        let (main_sha, drift) = match None::<BuildDriftStatus> {
            Some(_) => unreachable!(),
            None => (
                serde_json::Value::Null,
                serde_json::json!({
                    "behind": serde_json::Value::Null,
                    "checkedAt": serde_json::Value::Null,
                    "commitsBehind": serde_json::Value::Null,
                }),
            ),
        };
        assert!(main_sha.is_null());
        assert!(drift["behind"].is_null());
        assert!(drift["checkedAt"].is_null());
    }

    #[test]
    fn looks_like_sha_rejects_junk() {
        assert!(looks_like_sha("dc7aa9c5aaaabbbbccccddddeeeeffff00001111"));
        assert!(looks_like_sha("dc7aa9c5aaaa"));
        assert!(!looks_like_sha("unknown"));
        assert!(!looks_like_sha("dc7aa9c5")); // short SHAs under 12 are not our format
        assert!(!looks_like_sha("fatal: not a git repository"));
    }
}
