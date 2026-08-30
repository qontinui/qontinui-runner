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
//! On a slow interval (and once at startup) this module resolves the repo's
//! TRUNK tip SHA — `git ls-remote origin <trunk>` from the repo dir when
//! available, falling back to the local `git rev-parse origin/<trunk>` — and
//! diffs it against the embedded `gitSha` (prefix match: the embedded value
//! is a 12-char short SHA). The trunk comes from [`crate::git_trunk`]. The
//! result lands on `/health` as `mainSha` +
//! `buildDrift {behind, checkedAt, commitsBehind}` and a periodic WARN when
//! non-zero — the wire name stays `mainSha`, since coord and every `/health`
//! consumer reads it by that name; it is not a claim that the trunk is `main`.
//!
//! Resilience is the contract: a production install with no git repo (or no
//! network, or no `git` on PATH) serves nulls — every failure mode collapses
//! to `None`, never an error and never log spam (the WARN only fires on a
//! POSITIVE drift verdict, which requires git to have succeeded).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::process_helpers::{run_probe, ProbeOutcome};

/// Budget for one build-drift git read.
///
/// This one genuinely reaches the NETWORK: `resolve_trunk_sha` runs
/// `git ls-remote` before falling back to the local ref. `ls-remote` against
/// an unreachable or wedged remote is a classic never-returns, and the drift
/// check runs on a 900s timer through `spawn_blocking`, so without a bound one
/// bad remote removed a blocking-pool thread permanently on every tick. 60s is
/// well beyond a healthy `ls-remote` and far below the 900s interval.
const DRIFT_GIT_TIMEOUT: Duration = Duration::from_secs(60);
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
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
    /// The trunk tip's current full SHA, when resolvable. Named `main_sha`
    /// for the wire, not because the trunk is assumed to be `main`.
    pub main_sha: Option<String>,
    /// `Some(true)` only when trunk carries commits this binary does NOT
    /// have — i.e. `commits_behind > 0`. A build off a feature branch is
    /// DIVERGENT from trunk without being behind it; see `divergent`.
    ///
    /// This used to be `main_sha != embedded`, which made every non-tip build
    /// claim `behind: true` while `commitsBehind` sat at 0 — a body that
    /// contradicted itself. It feeds coord's `served_commits_behind`, so
    /// every feature-branch and agent-worktree build emitted a false drift
    /// signal.
    pub behind: Option<bool>,
    /// `git rev-list --count <embedded>..<main>` when cheaply available
    /// locally (requires both objects in the local repo); `None` otherwise.
    /// `behind` is DERIVED from this, so the two can no longer disagree.
    pub commits_behind: Option<u64>,
    /// `Some(true)` when the embedded SHA is not a prefix of `main_sha` —
    /// the old meaning of `behind`, kept as its own signal because "this
    /// binary is not the trunk tip" is still worth knowing about a build
    /// that is merely ahead.
    pub divergent: Option<bool>,
    /// `git rev-list --count <main>..<embedded>` — commits this binary has
    /// that trunk does not. Non-zero on a branch build, and the reason a
    /// divergent build can be zero commits behind.
    pub commits_ahead: Option<u64>,
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
                "divergent": s.divergent,
                "commitsAhead": s.commits_ahead,
            }),
        ),
        None => (
            serde_json::Value::Null,
            serde_json::json!({
                "behind": serde_json::Value::Null,
                "checkedAt": serde_json::Value::Null,
                "commitsBehind": serde_json::Value::Null,
                "divergent": serde_json::Value::Null,
                "commitsAhead": serde_json::Value::Null,
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
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(args).current_dir(repo);
    let ProbeOutcome::Captured(stdout) = run_probe(cmd, DRIFT_GIT_TIMEOUT, "build_drift: git")
    else {
        return None;
    };
    let s = String::from_utf8_lossy(&stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Resolve the trunk's current SHA: `ls-remote` (authoritative, needs
/// network) first, the local remote-tracking ref (last fetch) as fallback.
///
/// The trunk is resolved per repo ([`crate::git_trunk`]), not assumed to be
/// `main`. This is the mildest of the four sites that hardcoded it — both
/// rungs sit in an `.or_else` chain, so on a `master`-trunk checkout it
/// degraded to `None` ("drift unknown") rather than answering wrong — but a
/// permanent `None` here still reads as "we could not check" forever, and
/// `/health` `buildDrift` is exactly the signal operators use to decide
/// whether a fix is actually running on a box.
fn resolve_trunk_sha(repo: &Path) -> Option<String> {
    // The `main` guess is spelled here, visibly, rather than inside the
    // resolver: an unresolvable trunk means no LOCAL `origin/*` ref, which
    // says nothing about what the REMOTE would answer — and `ls-remote`
    // below needs no local ref at all. Dropping to `None` here would have
    // made a never-fetched checkout report "drift unknown" where the old
    // hardcoded `main` could still have answered.
    let branch = crate::git_trunk::resolve_trunk_branch(repo).unwrap_or_else(|| "main".to_string());
    git_output(repo, &["ls-remote", "origin", &branch])
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .or_else(|| git_output(repo, &["rev-parse", &format!("origin/{branch}")]))
        .filter(|s| looks_like_sha(s))
}

fn looks_like_sha(s: &str) -> bool {
    s.len() >= 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Pure comparison core (unit-testable without git): prefix-match the
/// embedded 12-char SHA against the trunk tip's full SHA. `None` when either
/// side is unknown (e.g. the embedded value is build.rs's `"unknown"`
/// fallback).
///
/// This answers DIVERGENCE — "this binary is not the trunk tip" — which is
/// strictly weaker than being behind it. Serving it AS `behind` is the bug
/// this function used to be named after.
fn compute_divergent(embedded: &str, main_sha: Option<&str>) -> Option<bool> {
    if !looks_like_sha(embedded) {
        return None;
    }
    main_sha.map(|m| !m.starts_with(embedded))
}

/// Reconcile a divergence verdict and a measured `embedded..main` count into
/// the `behind` flag actually served. The point is that the two can no longer
/// contradict each other:
///
/// - not divergent -> `behind: false`, 0 behind (built off the tip).
/// - divergent, count 0 -> `behind: FALSE`. Trunk holds nothing this build
///   lacks; it is on a branch off the tip, or ahead of it. This is exactly
///   the case that served `{"behind": true, "commitsBehind": 0}`.
/// - divergent, count n>0 -> `behind: true`.
/// - divergent, count unknown -> `behind` stays `true` (the conservative old
///   answer) with a null count. "Differs from trunk by an amount we could not
///   measure" is a warning, not a contradiction, so it is left standing.
fn reconcile_behind(divergent: Option<bool>, commits_behind: Option<u64>) -> Option<bool> {
    match divergent {
        None => None,
        Some(false) => Some(false),
        Some(true) => match commits_behind {
            Some(0) => Some(false),
            Some(_) => Some(true),
            None => Some(true),
        },
    }
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
            divergent: None,
            commits_ahead: None,
        };
    };

    let main_sha = resolve_trunk_sha(&repo);
    let divergent = compute_divergent(embedded, main_sha.as_deref());
    // Count FIRST, then derive `behind` from the count. The old code decided
    // `behind` from the SHA mismatch and only then counted, which is how the
    // two ended up disagreeing on every branch build.
    let commits_behind = match divergent {
        Some(true) => main_sha.as_deref().and_then(|m| {
            git_output(&repo, &["rev-list", "--count", &format!("{embedded}..{m}")])
                .and_then(|s| s.parse().ok())
        }),
        Some(false) => Some(0),
        None => None,
    };
    let commits_ahead = match divergent {
        Some(true) => main_sha.as_deref().and_then(|m| {
            git_output(&repo, &["rev-list", "--count", &format!("{m}..{embedded}")])
                .and_then(|s| s.parse().ok())
        }),
        Some(false) => Some(0),
        None => None,
    };
    let behind = reconcile_behind(divergent, commits_behind);

    BuildDriftStatus {
        checked_at,
        main_sha,
        behind,
        commits_behind,
        divergent,
        commits_ahead,
    }
}

/// Detached periodic drift check — runs once immediately at startup, then
/// every [`CHECK_INTERVAL`]. WARNs on each tick that finds non-zero drift.
pub async fn run_periodic() {
    loop {
        let status = spawn_blocking_tracked(check_once_blocking)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "build drift: check task panicked");
                BuildDriftStatus {
                    checked_at: chrono::Utc::now().timestamp_millis(),
                    main_sha: None,
                    behind: None,
                    commits_behind: None,
                    divergent: None,
                    commits_ahead: None,
                }
            });

        match (status.behind, status.main_sha.as_deref()) {
            (Some(true), Some(main)) => warn!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                main_sha = main,
                commits_behind = ?status.commits_behind,
                "build drift: this binary was NOT built from the trunk's current \
                 commit — shipped fixes may not be running here"
            ),
            (Some(false), _) => debug!(
                git_sha = env!("QONTINUI_GIT_SHA"),
                "build drift: binary matches the trunk tip"
            ),
            _ => debug!(
                "build drift: trunk tip unresolvable (no repo / no network) — reporting unknown"
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
    fn divergence_is_prefix_match_on_the_12_char_embedded_sha() {
        let main = "dc7aa9c5aaaabbbbccccddddeeeeffff00001111";
        // Embedded 12-char prefix of main → not divergent.
        assert_eq!(compute_divergent("dc7aa9c5aaaa", Some(main)), Some(false));
        // Different commit → divergent (which is NOT the same as behind).
        assert_eq!(compute_divergent("abcdef012345", Some(main)), Some(true));
    }

    #[test]
    fn unknown_states_collapse_to_none() {
        // build.rs fallback value must never produce a verdict.
        assert_eq!(compute_divergent("unknown", Some("dc7aa9c5aaaa")), None);
        // Unresolvable main → unknown.
        assert_eq!(compute_divergent("dc7aa9c5aaaa", None), None);
    }

    /// The reported defect: `{"behind": true, "commitsBehind": 0}`.
    ///
    /// A branch built off the current trunk tip is divergent (its own commits
    /// are not on trunk) but zero commits BEHIND it. Serving `behind: true`
    /// there contradicted the count in the same body and fed coord's
    /// `served_commits_behind` a false drift signal from every feature-branch
    /// and agent-worktree build.
    #[test]
    fn a_branch_build_that_is_zero_commits_behind_does_not_claim_to_be_behind() {
        assert_eq!(reconcile_behind(Some(true), Some(0)), Some(false));
    }

    #[test]
    fn behind_agrees_with_the_count_in_every_knowable_case() {
        // Built off the tip.
        assert_eq!(reconcile_behind(Some(false), Some(0)), Some(false));
        // Genuinely behind.
        assert_eq!(reconcile_behind(Some(true), Some(3)), Some(true));
        // Divergent but unmeasurable: a warning, not a contradiction — there
        // is no count for it to disagree with.
        assert_eq!(reconcile_behind(Some(true), None), Some(true));
        // Unknown stays unknown.
        assert_eq!(reconcile_behind(None, None), None);
        assert_eq!(reconcile_behind(None, Some(0)), None);
    }

    /// Guards the invariant rather than the individual cases: across every
    /// combination, a served `behind: true` must never sit next to a
    /// `commitsBehind` of 0.
    #[test]
    fn behind_true_is_never_served_alongside_zero_commits_behind() {
        for divergent in [None, Some(false), Some(true)] {
            for count in [None, Some(0u64), Some(1), Some(42)] {
                if reconcile_behind(divergent, count) == Some(true) {
                    assert_ne!(
                        count,
                        Some(0),
                        "behind:true served with commitsBehind:0 (divergent={divergent:?})"
                    );
                }
            }
        }
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
