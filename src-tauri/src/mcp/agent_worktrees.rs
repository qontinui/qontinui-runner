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
    allocate_and_materialize_with_claim, coord_ws_to_http, worktree_mode_enabled, AllocateError,
    RepoRequest,
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
    /// Plan 2026-05-18-agent-spawn-coordination Phase 3 — optional plan
    /// id used to derive a `ClaimKind::Phase` pre-flight claim. When
    /// supplied together with `phase`, the runner calls
    /// `POST /claims/acquire` with
    /// `kind=phase, resource_key=plan:<plan_id>:phase:<phase>` BEFORE
    /// `/agents/allocate`. On `Held` the response is `409 Conflict` with
    /// a structured body so the caller can surface the
    /// abort/wait/steal modal.
    #[serde(default)]
    pub plan_id: Option<String>,
    /// Phase 3 — sibling of `plan_id`. When both are present the claim
    /// kind is `Phase`; when only `declared_overlap_paths` is present,
    /// falls back to `FileGlob`; otherwise no pre-flight claim.
    #[serde(default)]
    pub phase: Option<String>,
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
                "machine_id not available: {} — run `qontinui_profile device init`",
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

    let canonical_paths =
        build_canonical_paths(&req.repos, &req.repo_canonical_paths).map_err(|e| {
            warn!("/agents/allocate-local: invalid repo slug: {e}");
            (
                StatusCode::BAD_REQUEST,
                Json(api_error(format!("invalid repo slug: {e}"))),
            )
        })?;
    let repo_reqs: Vec<RepoRequest> = req
        .repos
        .iter()
        .map(|r| RepoRequest {
            repo: r.repo.clone(),
            parent_sha: r.parent_sha.clone(),
        })
        .collect();

    match allocate_and_materialize_with_claim(
        &coord_http_base,
        &machine_id,
        &repo_reqs,
        req.intent.as_deref(),
        req.declared_overlap_paths.as_deref(),
        &canonical_paths,
        req.plan_id.as_deref(),
        req.phase.as_deref(),
    )
    .await
    {
        Ok(result) => {
            info!(
                "/agents/allocate-local ok: agent_id={} repos={}",
                result.agent_id,
                result.worktrees.len()
            );
            // Plan 2026-05-18-agent-spawn-coordination Phase 3 — spawn
            // a heartbeat task for the pre-acquired claim, if any. The
            // handle is registered in a process-global map keyed by
            // agent_id so the agent-completion path can release the
            // claim by lookup.
            if let Some(claim) = &result.active_claim {
                crate::agent_claims::register_active_claim(
                    &result.agent_id,
                    coord_http_base.clone(),
                    machine_id,
                    claim,
                );
            }
            // Spawn this agent's long-lived daemons (Row 9 Phase 3
            // pusher + Coordination Phase 6 dirty-poller) through the
            // single unified site. One shared, self-refreshing token
            // serves both. No-op when the allocation carried no JWT
            // (dev fallback / coord JWT keys unconfigured) — both
            // daemons' coord calls are JWT-gated. Best-effort: a
            // missing daemon degrades observability/durability, never
            // correctness.
            crate::agent_daemons::spawn_for_agent(&result, coord_http_base.clone(), machine_id);
            Ok(Json(ApiResponse::success(json!({
                "agent_id": result.agent_id,
                "worktrees": result.worktrees.iter().map(|w| json!({
                    "repo": w.repo,
                    "branch": w.branch,
                    "parent_sha": w.parent_sha,
                    "worktree_path": w.worktree_path.to_string_lossy(),
                })).collect::<Vec<_>>(),
                "active_claim": result.active_claim,
            }))))
        }
        Err(AllocateError::ClaimConflict(conflict)) => {
            warn!(
                "/agents/allocate-local claim held: kind={} key={} current_holder={}",
                conflict.kind, conflict.resource_key, conflict.current_holder
            );
            // 409 Conflict with structured body so the caller (orchestrator
            // or runner-side IPC translator) can surface the abort/wait/
            // steal modal. Per plan 2026-05-18-agent-spawn-coordination
            // Phase 3.
            Err((
                StatusCode::CONFLICT,
                Json(api_error(
                    serde_json::to_string(&serde_json::json!({
                        "error": "claim_conflict",
                        "kind": conflict.kind,
                        "resource_key": conflict.resource_key,
                        "current_holder": conflict.current_holder,
                        "intent": conflict.intent,
                    }))
                    .unwrap_or_else(|_| "claim_conflict".into()),
                )),
            ))
        }
        Err(AllocateError::Other(e)) => {
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

pub(crate) fn coord_http_base() -> Result<String, String> {
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

/// Resolve a canonical-checkout path per repo. Per-repo
/// `repo_canonical_paths` overrides win for non-standard checkouts; the
/// rest fall back to the unified
/// [`crate::agent_worktree::canonical_paths::default_canonical_path`]
/// resolver, which normalizes `owner/name` coord slugs into the runner's
/// flat `<root>/<name>/` layout. A malformed slug (empty / trailing-slash)
/// is surfaced as `Err` so the HTTP handler returns a 400 instead of
/// attempting `git worktree add` on `<root>/`.
fn build_canonical_paths(
    repos: &[AllocateLocalRepo],
    override_map: &HashMap<String, String>,
) -> Result<HashMap<String, PathBuf>, String> {
    let mut out: HashMap<String, PathBuf> = HashMap::new();
    for r in repos {
        let path = if let Some(p) = override_map.get(&r.repo) {
            PathBuf::from(p)
        } else {
            crate::agent_worktree::canonical_paths::default_canonical_path(&r.repo)?
        };
        out.insert(r.repo.clone(), path);
    }
    Ok(out)
}

pub fn routes() -> axum::Router<Arc<ApiState>> {
    use axum::routing::post;
    axum::Router::new().route("/agents/allocate-local", post(post_allocate_local))
}
