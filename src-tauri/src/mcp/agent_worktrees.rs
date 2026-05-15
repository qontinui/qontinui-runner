//! HTTP endpoint that drives the worktree-per-agent spawn path
//! (Coordination Phase 1).
//!
//! `POST /agents/allocate-local` — runner-side wrapper that:
//!
//! 1. Calls qontinui-coord `POST /agents/allocate` (HTTP).
//! 2. Runs `git worktree add` for each returned per-repo row.
//! 3. Returns `{ agent_id, worktrees: [{repo, branch, parent_sha,
//!    worktree_path}] }` so the caller (PTY-spawn, slash-command, UI)
//!    can use `worktrees[].worktree_path` as the terminal CWD.
//!
//! Gated behind env var `QONTINUI_AGENT_WORKTREE_MODE` (default off).
//! When off, the endpoint returns 503 with an actionable message.
//!
//! The endpoint exists only on the runner because each runner is the
//! "spawning host" in plan §4.1 — it owns the canonical checkouts and
//! the filesystem the worktrees live on. Cross-machine spawn is Phase 5
//! territory and uses the same coord `/agents/allocate` underneath.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use crate::agent_worktree::{
    allocate_and_materialize, coord_ws_to_http, worktree_mode_enabled, RepoRequest,
};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// Body of `POST /agents/allocate-local`.
#[derive(Debug, Deserialize)]
pub struct AllocateLocalRequest {
    /// One per repo the agent will work in. The runner reads
    /// `parent_sha` from the caller (so callers that depend on a
    /// specific HEAD can pin it). Empty list → 400.
    pub repos: Vec<AllocateLocalRepo>,
    /// Optional opaque human-readable intent. Echoed back into
    /// `coord.agent_worktrees.intent`.
    #[serde(default)]
    pub intent: Option<String>,
    /// Phase 1B: optional pre-derived overlap paths. When supplied,
    /// coord persists them verbatim (skipping LLM derivation).
    #[serde(default)]
    pub declared_overlap_paths: Option<Vec<String>>,
    /// Optional override of the canonical-checkout path for each repo.
    /// Map of `repo` slug to absolute path. When not provided for a
    /// given repo, defaults to `D:/qontinui-root/<repo>/` (Windows)
    /// or `$HOME/qontinui-root/<repo>/` (POSIX).
    #[serde(default)]
    pub repo_canonical_paths: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct AllocateLocalRepo {
    pub repo: String,
    pub parent_sha: String,
}

/// `POST /agents/allocate-local` handler.
pub async fn post_allocate_local(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<AllocateLocalRequest>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    if !worktree_mode_enabled() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(api_error(
                "agent_worktree_mode is off (set QONTINUI_AGENT_WORKTREE_MODE=1 \
                 on the runner to enable)",
            )),
        ));
    }
    if req.repos.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("repos must not be empty")),
        ));
    }

    let machine_id = read_machine_id().map_err(|e| {
        warn!("/agents/allocate-local: machine_id load failed: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!(
                "machine_id not available: {} — run `qontinui_profile machine init`",
                e
            ))),
        )
    })?;

    let coord_http_base = coord_http_base().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("coord URL not configured: {}", e))),
        )
    })?;

    let canonical_paths = build_canonical_paths(&req.repos, &req.repo_canonical_paths);
    let repo_reqs: Vec<RepoRequest> = req
        .repos
        .iter()
        .map(|r| RepoRequest {
            repo: r.repo.clone(),
            parent_sha: r.parent_sha.clone(),
        })
        .collect();

    match allocate_and_materialize(
        &coord_http_base,
        &machine_id,
        &repo_reqs,
        req.intent.as_deref(),
        req.declared_overlap_paths.as_deref(),
        &canonical_paths,
    )
    .await
    {
        Ok(result) => {
            info!(
                "/agents/allocate-local ok: agent_id={} repos={}",
                result.agent_id,
                result.worktrees.len()
            );
            Ok(Json(ApiResponse::success(json!({
                "agent_id": result.agent_id,
                "worktrees": result.worktrees.iter().map(|w| json!({
                    "repo": w.repo,
                    "branch": w.branch,
                    "parent_sha": w.parent_sha,
                    "worktree_path": w.worktree_path.to_string_lossy(),
                })).collect::<Vec<_>>(),
            }))))
        }
        Err(e) => {
            warn!("/agents/allocate-local failed: {e}");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(api_error(format!("allocate_and_materialize: {}", e))),
            ))
        }
    }
}

fn read_machine_id() -> Result<uuid::Uuid, String> {
    let path = dirs::home_dir()
        .map(|h| h.join(".qontinui").join("machine.json"))
        .ok_or_else(|| "no HOME dir".to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    let id_str = v
        .get("machine_id")
        .and_then(|s| s.as_str())
        .ok_or_else(|| format!("{}: missing machine_id", path.display()))?;
    uuid::Uuid::parse_str(id_str).map_err(|e| format!("invalid UUID: {}", e))
}

fn coord_http_base() -> Result<String, String> {
    // Source-of-truth chain: env `COORD_HTTP_URL` → profile `coord_url`
    // (ws→http) → default `http://localhost:9870`.
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let profile = qontinui_runner_lib::profiles::load();
    if let Some(ws) = profile.coord_url.as_deref() {
        return Ok(coord_ws_to_http(ws));
    }
    Ok("http://localhost:9870".to_string())
}

fn build_canonical_paths(
    repos: &[AllocateLocalRepo],
    override_map: &HashMap<String, String>,
) -> HashMap<String, PathBuf> {
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    for r in repos {
        let path = if let Some(p) = override_map.get(&r.repo) {
            PathBuf::from(p)
        } else {
            default_canonical_path(&r.repo)
        };
        out.insert(r.repo.clone(), path);
    }
    out
}

#[cfg(target_os = "windows")]
fn default_canonical_path(repo: &str) -> PathBuf {
    PathBuf::from(format!("D:/qontinui-root/{}", repo))
}

#[cfg(not(target_os = "windows"))]
fn default_canonical_path(repo: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/qontinui-root/{}", home, repo))
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/agents/allocate-local", post(post_allocate_local))
}
