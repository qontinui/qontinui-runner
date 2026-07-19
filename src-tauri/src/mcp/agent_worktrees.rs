//! Agent-worktree HTTP surface + coord URL resolution.
//!
//! ## `/agent-worktrees/*` — on-demand cleanup (plan
//! `2026-07-19-worktree-cleanup-lifecycle-tracking`, Phase 4)
//!
//! Two routes, one executor:
//!
//! | Route | Purpose |
//! |---|---|
//! | `GET /agent-worktrees/reclaimable` | Project coord's reap decision for THIS device — the reapable set PLUS every blocked worktree with the guard that blocked it. |
//! | `POST /agent-worktrees/reclaim` | Remove ONLY coord-cleared, locally-reverified worktrees. Returns `{removed, skipped:[{id, reason}]}`. |
//!
//! ### Why NOT `/worktrees/*`
//!
//! `/worktrees` on this same `:9876` API is **already taken** by an
//! unrelated, older subsystem ([`crate::mcp::worktrees`] over
//! [`crate::worktree`] — the task-run/workflow "isolated run" worktrees under
//! `.worktrees`), which already exposes a differently-meaning
//! **`POST /worktrees/remove`**. Shipping `POST /worktrees/reclaim` next to it
//! would invite an operator or an agent to fire the wrong destructive route.
//! These are AGENT worktrees (coord's `agent_worktrees` ledger,
//! `<repo>-wt-*` / `qontinui-worktrees/`) — a different resource, hence a
//! different namespace.
//!
//! ### Triggers
//!
//! * **Primary — the runner's "Worktrees" UI panel**
//!   (`src/components/worktrees/WorktreesPanel.tsx`): the operator sees
//!   exactly what will be removed, and why each refusal happened, BEFORE
//!   pressing "Clean up safe worktrees".
//! * **Secondary — any agent**, for free: plain HTTP on the 127.0.0.1-bound
//!   `:9876` API with no auth, so an agent in a runner terminal does `GET` →
//!   show the operator → `POST`, no extra machinery. `POST {"dryRun": true}`
//!   re-runs every guard and reports the verdict without touching disk.
//!
//! Both drive the SAME endpoint, which runs the SAME removal path
//! ([`crate::agent_worktree::reclaim::execute_steps`]) the silent background
//! poller uses — one place a worktree removal can happen. Arming that silent
//! loop (coord-side `COORD_WORKTREE_RECLAIM_ENABLED`) is NOT required and is
//! deliberately untouched by this surface.
//!
//! **axum 0.8** — this crate panics at Router build on a `:param` literal, so
//! any future path param here must use braces (`{id}`). These two routes are
//! static.
//!
//! ## Coord URL resolution
//!
//! The worktree-allocation flow itself lives in
//! [`crate::agent_worktree`] and is driven in-process by
//! [`crate::agent_worktree::isolated_edit`] (the terminal-spawn /
//! slash-command path). This module also retains the shared
//! coord-base resolver that several call sites depend on
//! (`file_registry`, `terminal::coord_warn`, `commands::productivity`,
//! `commands::claims`, `fleet`, …).
//!
//! Historical note: a `POST /agents/allocate-local` HTTP endpoint used
//! to live here as a runner-side wrapper over coord's `/agents/allocate`.
//! It never acquired a caller — the in-process `isolated_edit` facade
//! superseded it — so it was removed along with the unused no-claim
//! `allocate_and_materialize` wrapper.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};

use crate::agent_worktree::on_demand::{self, ReclaimRequest};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/agent-worktrees/reclaimable", get(reclaimable_handler))
        .route("/agent-worktrees/reclaim", post(reclaim_handler))
}

/// `GET /agent-worktrees/reclaimable`
///
/// ```jsonc
/// { "success": true, "data": {
///     "device_id": "…",
///     "coord_reachable": true,
///     "coord_error": null,
///     "remove_armed": false,      // informational; this path does not need it
///     "rejunction_armed": false,
///     "canonical_excluded": 12,   // canonical checkouts filtered out (never reapable)
///     "items": [
///       { "id": "d:/qontinui-root/qontinui-runner-wt-a",
///         "worktree_path": "D:/qontinui-root/qontinui-runner-wt-a",
///         "repo": "qontinui-runner", "branch": "feat/x",
///         "status": "reapable", "reason": null, "reason_detail": null,
///         "is_dirty": false, "building": false, "pinned": false,
///         "landed_in_main": true, "attributable_bytes": 4096,
///         "junctioned_paths": ["node_modules"],
///         "coord_reason": "worktree:lifecycle:pr_merged" },
///       { "id": "…-wt-b", "status": "blocked", "reason": "dirty",
///         "reason_detail": "Uncommitted work in this tree (G1) — …" }
///     ],
///     "summary": { "reapable": 1, "blocked": 1, "reclaimable_bytes": 4096 } } }
/// ```
async fn reclaimable_handler(
    State(_state): State<Arc<ApiState>>,
) -> Result<Json<ApiResponse<on_demand::Survey>>, (StatusCode, Json<ApiResponse<()>>)> {
    match on_demand::survey().await {
        Ok(survey) => Ok(Json(ApiResponse::success(survey))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("worktree survey failed: {e}"))),
        )),
    }
}

/// `POST /agent-worktrees/reclaim`
///
/// Request (all fields optional):
/// ```jsonc
/// { "ids": ["d:/qontinui-root/qontinui-runner-wt-a"], "dryRun": false }
/// ```
/// `ids` omitted ⇒ every currently-reapable worktree. Ids can only NARROW the
/// set coord cleared — the survey is always re-derived server-side, so a
/// client can never widen it.
///
/// Response:
/// ```jsonc
/// { "success": true, "data": {
///     "removed": [{ "id": "…", "worktree_path": "…", "repo": "…",
///                   "freed_bytes": 4096, "dry_run": false }],
///     "skipped": [{ "id": "…", "worktree_path": "…", "reason": "building",
///                   "detail": "A build is in flight here (G6) — …" }],
///     "dry_run": false } }
/// ```
/// `reason` is one of `dirty` (G1) | `pinned` | `session-live` (G3) |
/// `building` (G6) | `not-landed` (G2) | `main-merge` (G4) | `grace` (G5) |
/// `not-a-candidate` | `not-cleared` | `coord-unreachable` | `absent` |
/// `not-reapable` | `error`. Every refusal is listed — never a silent drop.
async fn reclaim_handler(
    State(_state): State<Arc<ApiState>>,
    body: Option<Json<ReclaimRequest>>,
) -> Result<Json<ApiResponse<on_demand::ReclaimOutcome>>, (StatusCode, Json<ApiResponse<()>>)> {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    match on_demand::reclaim_now(req).await {
        Ok(outcome) => Ok(Json(ApiResponse::success(outcome))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("worktree reclaim failed: {e}"))),
        )),
    }
}

// ============================================================================
// Coord URL resolution
// ============================================================================

pub(crate) fn coord_http_base() -> Result<String, String> {
    // Source-of-truth chain: env `COORD_HTTP_URL` → profile `coord_url`
    // (ws→http) → tier-aware default (prod coord on a hosted runner,
    // dev-localhost guess otherwise). Delegates to the shared policy fn. This
    // resolver never errored before (it always fell back to a default-Ok), so
    // the `Result` contract is preserved by always returning Ok.
    Ok(qontinui_runner_lib::profiles::coord_base_with_source().0)
}
