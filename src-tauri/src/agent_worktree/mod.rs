//! Worktree-per-agent spawn path (Coordination Phase 1).
//!
//! Plan reference:
//! `D:/qontinui-root/plans/2026-05-14-branch-per-agent-coordination-plan.md`
//! §4.1. New code path that, on session creation, calls qontinui-coord's
//! `POST /agents/allocate`, materializes the per-repo worktrees via
//! `git worktree add`, and returns the agent_id + per-repo materialized
//! paths so the caller (PTY-spawn, slash-command, UI) can use them as
//! CWD.
//!
//! Gated behind the `QONTINUI_AGENT_WORKTREE_MODE` env var (default off).
//! When off, this module is dead code — the existing shared-tree spawn
//! path stays exclusively active. Reversible per the plan's Phase 7
//! commit ("feature-flag-flip the new spawn path on for everyone,
//! monitor a week, then delete").
//!
//! ## Scope (Phase 1)
//!
//! - Call coord `/agents/allocate` with `{ machine_id, repos: [{repo,
//!   parent_sha}], intent? }`.
//! - `git worktree add <suggested-path> -b <branch> <parent_sha>` for
//!   each returned worktree row.
//! - Return materialized rows.
//!
//! ## Not yet in scope
//!
//! - Cross-repo Cargo.toml path-dep rewriting. Memory
//!   `feedback_worktree_path_dep_hooks` documents the gotcha for
//!   committed rewrites; uncommitted rewrites get stashed by the
//!   pre-commit cargo hook. Phase 1 surfaces the worktrees with the
//!   path deps pointing at the **canonical** sibling tree (not the
//!   sibling worktree). Cross-repo work that needs the sibling
//!   worktree's HEAD is a follow-up (see tracker Row 5 "Cross-repo
//!   path-deps").
//! - Status lifecycle (`allocated → active`). Phase 1 writes the row
//!   in `allocated` state via coord, and the runner doesn't yet
//!   transition it. Phase 3+ (merge proposal API) drives the rest of
//!   the state machine.
//! - Cleanup of materialized worktrees. The sweeper on coord prunes
//!   `coord.agent_worktrees` rows; pruning the on-disk worktree
//!   directories is Phase 6+ territory.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::worktree::run_git_command;

/// Env var that turns the new spawn path on. Default off — `feature
/// flag agent_worktree_mode` per plan §5 Phase 1.
const FLAG_ENV: &str = "QONTINUI_AGENT_WORKTREE_MODE";

/// Returns true iff the worktree-per-agent spawn mode is enabled.
/// Accepts the usual truthy values; anything else (including unset) is
/// false.
pub fn worktree_mode_enabled() -> bool {
    matches!(
        std::env::var(FLAG_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// A repo the caller wants a worktree for, paired with the commit the
/// worktree should branch off of. The runner is the host so it
/// resolves `parent_sha` from its own checkout — coord doesn't
/// re-resolve.
#[derive(Debug, Clone, Serialize)]
pub struct RepoRequest {
    pub repo: String,
    pub parent_sha: String,
}

/// A single materialized worktree as returned by `allocate_and_materialize`.
/// `worktree_path` is the actual on-disk path the runner created (matches
/// coord's `suggested_path` in the happy path, but the runner is allowed
/// to deviate — e.g. tighter disk).
#[derive(Debug, Clone, Serialize)]
pub struct MaterializedWorktree {
    pub repo: String,
    pub branch: String,
    pub parent_sha: String,
    pub worktree_path: PathBuf,
}

/// Result of a full allocate + materialize round-trip.
///
/// Row 9 Phase 2 added `token`/`token_jti`/`token_exp`: the scoped
/// JWT coord issued at allocation. Phase 3's pusher daemon
/// (`crate::agent_pusher`) consumes these to authenticate
/// pushes to the coord-hosted git origin. Empty `token` (JWT keys
/// not configured on coord) means "skip pusher spawn for this
/// allocation."
#[derive(Debug, Clone, Serialize)]
pub struct AllocateResult {
    pub agent_id: String,
    pub worktrees: Vec<MaterializedWorktree>,
    pub token: String,
    pub token_jti: uuid::Uuid,
    pub token_exp: i64,
}

/// Coord's JSON response shape for `POST /agents/allocate`. Mirrored
/// here so we don't have to share a crate just for two structs.
#[derive(Debug, Deserialize)]
struct CoordAllocateResponse {
    agent_id: String,
    worktrees: Vec<CoordAllocatedWorktree>,
    /// Row 9 Phase 2 — scoped JWT covering all branches in this
    /// allocation. Empty when coord's JWT keys aren't configured
    /// (dev fallback).
    #[serde(default)]
    token: String,
    #[serde(default)]
    token_jti: Option<uuid::Uuid>,
    #[serde(default)]
    token_exp: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CoordAllocatedWorktree {
    repo: String,
    branch: String,
    parent_sha: String,
    worktree_path: String,
    #[allow(dead_code)]
    status: String,
}

/// Call coord's `/agents/allocate` and then `git worktree add` for each
/// returned row.
///
/// `coord_http_base` is the HTTP base URL of qontinui-coord (e.g.
/// `http://localhost:9870`). The runner's profile stores `coord_url` as
/// a `ws://` URL — callers should convert via [`coord_ws_to_http`]
/// before calling here.
///
/// `repo_canonical_paths` maps `repo` slug to the canonical checkout
/// path on the runner's host. The runner's host typically holds one
/// canonical checkout per repo at `D:/qontinui-root/<repo>/`; this map
/// is the dependency injection point so tests can substitute scratch
/// repos.
///
/// On success returns `AllocateResult`. On any error, returns a
/// caller-readable string. Partial failure is handled at the boundary:
/// if any `git worktree add` fails after coord has minted rows, the
/// partial materialization stops; coord's sweeper will eventually
/// reclaim the unused rows once they age into `abandoned`.
pub async fn allocate_and_materialize(
    coord_http_base: &str,
    machine_id: &uuid::Uuid,
    repos: &[RepoRequest],
    intent: Option<&str>,
    declared_overlap_paths: Option<&[String]>,
    repo_canonical_paths: &std::collections::HashMap<String, PathBuf>,
) -> Result<AllocateResult, String> {
    if !worktree_mode_enabled() {
        return Err(format!(
            "{} is not enabled; spawn path is disabled",
            FLAG_ENV
        ));
    }
    if repos.is_empty() {
        return Err("repos must not be empty".to_string());
    }

    // Pre-flight: every requested repo must have a canonical path the
    // runner can `git worktree add` from. Surface this before bothering
    // coord with the request.
    for r in repos {
        if !repo_canonical_paths.contains_key(&r.repo) {
            return Err(format!(
                "no canonical checkout known for repo '{}' — pass it in \
                 repo_canonical_paths",
                r.repo
            ));
        }
    }

    // Phase 1B: declared_overlap_paths is optional; when present, coord
    // skips its LLM-based derivation step and uses our paths directly.
    // When absent, coord derives from `intent` (or falls back to empty).
    let body = serde_json::json!({
        "machine_id": machine_id.to_string(),
        "repos": repos.iter().map(|r| serde_json::json!({
            "repo": r.repo,
            "parent_sha": r.parent_sha,
        })).collect::<Vec<_>>(),
        "intent": intent,
        "declared_overlap_paths": declared_overlap_paths,
    });

    let url = format!("{}/agents/allocate", coord_http_base.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "POST {url} returned {} — body: {}",
            status.as_u16(),
            body_text
        ));
    }
    let coord_resp: CoordAllocateResponse = resp
        .json()
        .await
        .map_err(|e| format!("decode coord response: {e}"))?;

    info!(
        "coord allocated agent_id={} repos={}",
        coord_resp.agent_id,
        coord_resp.worktrees.len()
    );

    let mut materialized: Vec<MaterializedWorktree> = Vec::with_capacity(repos.len());
    for w in coord_resp.worktrees {
        let canonical = repo_canonical_paths
            .get(&w.repo)
            .ok_or_else(|| format!("missing canonical path for repo '{}'", w.repo))?;
        let target = PathBuf::from(&w.worktree_path);

        // Ensure the parent dir exists. `git worktree add` will create
        // the leaf, but the parent (`D:/qontinui-root.wt/<agent>/`)
        // doesn't exist on first allocation.
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "create parent dir {} for worktree {}: {}",
                    parent.display(),
                    w.repo,
                    e
                )
            })?;
        }

        // `git -C <canonical> worktree add <target> -b <branch> <parent_sha>`.
        // Plan §4.1 step 4 spelled this exact command.
        let target_str = target.to_string_lossy().to_string();
        let args: [&str; 6] = [
            "worktree",
            "add",
            &target_str,
            "-b",
            &w.branch,
            &w.parent_sha,
        ];
        match run_git_command(canonical, &args) {
            Ok(stdout) => {
                info!(
                    "git worktree add ok: repo={} branch={} path={} stdout={}",
                    w.repo,
                    w.branch,
                    target.display(),
                    stdout.trim()
                );
            }
            Err(e) => {
                warn!(
                    "git worktree add failed: repo={} branch={} path={}: {}",
                    w.repo,
                    w.branch,
                    target.display(),
                    e
                );
                return Err(format!(
                    "git worktree add for repo '{}' (branch {}) failed: {}",
                    w.repo, w.branch, e
                ));
            }
        }

        materialized.push(MaterializedWorktree {
            repo: w.repo,
            branch: w.branch,
            parent_sha: w.parent_sha,
            worktree_path: target,
        });
    }

    Ok(AllocateResult {
        agent_id: coord_resp.agent_id,
        worktrees: materialized,
        token: coord_resp.token,
        token_jti: coord_resp.token_jti.unwrap_or(uuid::Uuid::nil()),
        token_exp: coord_resp.token_exp.unwrap_or(0),
    })
}

/// Convert a `ws://` or `wss://` coord URL into the matching HTTP base.
/// Profiles store `coord_url` as a WebSocket URL because the `/ws`
/// endpoint is the dominant runner-side use case; HTTP callers (this
/// module, build_events POSTs) flip the scheme.
pub fn coord_ws_to_http(coord_url: &str) -> String {
    if let Some(rest) = coord_url.strip_prefix("ws://") {
        format!("http://{}", rest)
    } else if let Some(rest) = coord_url.strip_prefix("wss://") {
        format!("https://{}", rest)
    } else {
        coord_url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_off_by_default() {
        // Tests run with no env var set unless tested explicitly. We
        // don't mutate process env here because the runner test
        // harness mutates env globally (memory
        // `feedback_env_var_tests_serialize`); this assertion holds
        // as long as no test sets QONTINUI_AGENT_WORKTREE_MODE before
        // this runs.
        if std::env::var(FLAG_ENV).is_err() {
            assert!(!worktree_mode_enabled());
        }
    }

    #[test]
    fn coord_ws_to_http_swaps_scheme() {
        assert_eq!(coord_ws_to_http("ws://h:9870"), "http://h:9870");
        assert_eq!(coord_ws_to_http("wss://h:9870"), "https://h:9870");
        assert_eq!(coord_ws_to_http("http://h:9870"), "http://h:9870");
        assert_eq!(coord_ws_to_http("https://h:9870"), "https://h:9870");
    }
}
