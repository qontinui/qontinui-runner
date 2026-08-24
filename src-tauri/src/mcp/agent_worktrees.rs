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
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};

use crate::agent_worktree::on_demand::{self, ReclaimRequest, SurveyQuery};
use crate::mcp::types::{api_error, ApiResponse, ApiState};

// ============================================================================
// Routes
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/agent-worktrees/reclaimable", get(reclaimable_handler))
        .route("/agent-worktrees/reclaim", post(reclaim_handler))
        // WIP-custody orphan report (plan
        // `2026-08-22-wip-custody-rebuild-survivable-attribution`, Phase 3):
        // the worktrees holding real uncommitted work whose owning session is
        // NOT live, each with its WIP summary and a ready-to-run resume line.
        .route("/agent-worktrees/wip-orphans", get(wip_orphans_handler))
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
///     "summary": {
///       "reapable": 1, "blocked": 1, "reclaimable_bytes": 4096,
///       // Free space — sampled on its OWN 60s tick, NOT by the census walk,
///       // so these fields answer even while `census_status` is "pending".
///       "volumes": [ { "volume": "D:", "total_bytes": 4000, "free_bytes": 93 } ],
///       "volumes_status": "fresh", // "pending" (UNKNOWN) | "fresh" | "stale"
///       "volumes_observed_at": "2026-08-16T17:49:48Z",
///       "volumes_age_secs": 12,
///       "free_bytes_total": 93,    // null — never 0 — while "pending"
///       "total_bytes_total": 4000,
///       "volumes_note": "Free space as of 12s ago, across 1 volume."
///     },
///     "census_status": "fresh",   // "pending" | "fresh" | "stale"
///     "census_taken_at": "2026-07-19T11:49:48Z",
///     "census_age_secs": 214,
///     "census_build_ms": 802431,
///     "census_refreshing": false,
///     "census_note": "Disk state as of 3m 34s ago. Removal always re-checks live disk…" } }
/// ```
///
/// ### Bounded by construction
///
/// The disk census is a MULTI-MINUTE walk, so this route reads the snapshot
/// published by the periodic census task rather than rebuilding one per
/// request (which made it hang indefinitely). It answers within the bounded
/// coord pull (15s) plus a ≤10s snapshot wait, always — a cold start with no
/// snapshot yet returns `census_status: "pending"` with an empty `items` and a
/// note saying so, never an implied "nothing to clean up".
///
/// ### The free-space half is independent (INV-D1)
///
/// `summary.volumes*` comes from the 60s volume publisher
/// (`agent_worktree::census::spawn_volume_publisher`), which shares nothing
/// with the census walk, coord, or any arming flag. So "how much disk is
/// left?" is answered on the cold-start path, mid-walk, mid-build and with
/// coord unreachable — the conditions under which measurement matters most.
/// `volumes_status: "pending"` with `free_bytes_total: null` is UNKNOWN; it
/// is never rendered as zero free space.
///
/// ### Query params
///
/// * `?refresh=1` — kick a fresh census walk in the BACKGROUND. The response
///   still comes from the cached snapshot (with `census_refreshing: true`);
///   the walk is never awaited beyond `waitSecs`.
/// * `?waitSecs=N` — bounded override of the snapshot wait (capped at 10).
async fn reclaimable_handler(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<SurveyQuery>,
) -> Result<Json<ApiResponse<on_demand::Survey>>, (StatusCode, Json<ApiResponse<()>>)> {
    match on_demand::survey(query).await {
        Ok(survey) => Ok(Json(ApiResponse::success(survey))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("worktree survey failed: {e}"))),
        )),
    }
}

/// `GET /agent-worktrees/wip-orphans[?refresh=1&waitSecs=N]`
///
/// The report the operator's complaint asks for: *"there is probably WIP in
/// many of these sessions and I can't identify easily which session the WIP
/// refers to."*
///
/// ```jsonc
/// { "success": true, "data": {
///     "predicate": "is_dirty AND owner_live != true AND (no custody record OR …)",
///     "scanned": 1250, "wip_total": 170,
///     "orphans": [
///       { "worktree_path": "D:/qontinui-root/_wt/foo",
///         "session_label": "amber-otter (session aaaa1111)",
///         "attribution_source": "custody_record",
///         "attribution_confidence": "strong",
///         "orphan_reason": "custody-stale",
///         "last_seen": "2026-08-21T09:12:04Z", "last_seen_age_secs": 190_000,
///         "wip_summary": "Uncommitted work, snapshotted to refs/wip/aaaa1111 (2.1 GB)…",
///         "resume_command": "cd \"D:/…/foo\" && CLAUDE_CONFIG_DIR=\"C:/claude/.claude-gmail\" claude --resume aaaa1111-…",
///         "recover_wip_command": "git -C \"D:/…/foo\" stash apply 9f2c…" }
///     ],
///     "session_roots_scanned": 5, "coord_ownership_reachable": true } }
/// ```
///
/// **Read-only.** It removes nothing, pins nothing and clears nothing — the
/// plan's scope fence is explicit that no phase here removes a worktree, a
/// branch or a target directory.
///
/// Same bounded-by-construction posture as
/// [`reclaimable_handler`]: it reads the cached census snapshot, and a
/// `census_status: "pending"` report says "not known yet", never "nothing is
/// orphaned".
async fn wip_orphans_handler(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<SurveyQuery>,
) -> Result<Json<ApiResponse<on_demand::WipOrphanReport>>, (StatusCode, Json<ApiResponse<()>>)> {
    match on_demand::wip_orphans(query).await {
        Ok(report) => Ok(Json(ApiResponse::success(report))),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("wip-orphan report failed: {e}"))),
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
