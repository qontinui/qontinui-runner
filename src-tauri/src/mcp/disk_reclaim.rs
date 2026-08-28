//! `GET /disk/reclaimable` — the runner-local disk-reclaim preview.
//!
//! Plan: `plans/2026-08-07-product-disk-monitoring-and-cleanup.md`, Phase 2
//! step 1. Modelled directly on `GET /agent-worktrees/reclaimable`
//! ([`super::agent_worktrees`]): items carrying `status` /`reason` /
//! `reason_detail` / `class` / `bytes` / `last_used_at`, a `summary` with
//! `reclaimable_bytes` **per class**, and the freshness triple
//! `census_status` / `census_age_secs` / `census_note`.
//!
//! ## Why HTTP and not Tauri IPC
//!
//! The web UI reaches the runner over loopback HTTP only
//! (`qontinui-web/frontend/src/lib/runner/api-client.ts` → `:9876`), so a Tauri
//! command would be unreachable from the surface this preview exists for. The
//! worktree cleanup panel made the same choice for the same reason.
//!
//! ## Why `/disk/*` and not `/agent-worktrees/*`
//!
//! The resource is different: `/agent-worktrees/*` is about WORKTREES (coord's
//! ledger, git state, a destructive `POST …/reclaim` next door), while this is
//! about **cargo build directories** anywhere on the machine, including inside
//! canonical checkouts that are not worktrees at all and never reclaimable.
//! Sharing a namespace would put a read-only preview one path segment away from
//! an unrelated destructive route.
//!
//! ## INV-D1
//!
//! This route has **no destructive twin in this phase and no preconditions**.
//! It never checks an arming flag, never contacts coord, and never consults a
//! global build-in-flight condition — a build in flight changes one item's
//! verdict, never whether there is an answer. See
//! [`crate::agent_worktree::disk_survey`] for the invariant and its regression
//! test.
//!
//! **axum 0.8** — this crate panics at Router build on a `:param` literal; this
//! route is static.

use std::sync::Arc;

use axum::{extract::Query, extract::State, response::Json, routing::get, Router};

use crate::agent_worktree::disk_survey::{self, DiskSurveyQuery};
use crate::mcp::types::{ApiResponse, ApiState};

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/disk/reclaimable", get(disk_reclaimable_handler))
}

/// `GET /disk/reclaimable`
///
/// ```jsonc
/// { "success": true, "data": {
///     "workspace_root": "D:/qontinui-root",
///     "items": [
///       { "id": "d:/qontinui-root/_targets/coord-x",
///         "path": "D:/qontinui-root/_targets/coord-x",
///         "class": "container",              // in-repo-canonical | sibling-worktree | container | sibling-nongit
///         "status": "reclaimable",           // "reclaimable" | "blocked"
///         "reason": null, "reason_detail": null,
///         "bytes": 22548578304,              // null (NEVER 0) when unreadable
///         "bytes_partial": false,
///         "last_used_at": "2026-08-11T03:12:44Z",
///         "repo_root": null,
///         "verb": "orphan-target-reaper" },  // null ⇒ no v1 verb for this class
///       { "id": "…/qontinui-runner/target",
///         "class": "in-repo-canonical", "status": "blocked",
///         "reason": "report-only", "reason_detail": "Inside a canonical repo checkout…" }
///     ],
///     "summary": {
///       "roots": 244, "reclaimable": 61, "blocked": 183,
///       "total_bytes": 3834138115, "reclaimable_bytes": 1039382937,
///       "report_only_bytes": 1793044582,   // the class with no verb — the biggest number here
///       "bytes_incomplete": false,
///       "by_class": [ { "class": "in-repo-canonical", "roots": 41, "bytes": …,
///                       "reclaimable_roots": 0, "reclaimable_bytes": 0,
///                       "verb": null, "note": "…" }, … ],
///       // Free space — the 60s publisher, independent of the walk above.
///       "volumes": [ { "volume": "D:", "total_bytes": 4000, "free_bytes": 93 } ],
///       "volumes_status": "fresh", "volumes_age_secs": 12,
///       "free_bytes_total": 93, "volumes_note": "Free space as of 12s ago…" },
///     "census_status": "fresh",   // "pending" | "fresh" | "stale" | "unavailable"
///     "census_taken_at": "2026-08-18T09:11:02Z",
///     "census_age_secs": 214,
///     "census_build_ms": 88231,
///     "census_refreshing": false,
///     "census_note": "Disk state as of 3m 34s ago, from an 88231 ms walk of …",
///     "scan": { "dirs_visited": 41022, "truncated": false,
///               // A capped SAMPLE; `read_errors_total` is the real count.
///               "read_errors": [], "read_errors_total": 0,
///               "roots_with_unknown_bytes": 0, "roots_with_partial_bytes": 0 } } }
/// ```
///
/// ### Bounded by construction
///
/// Sizing every cargo target root is a multi-minute walk, so this reads the
/// snapshot published by the periodic surveyor rather than rebuilding one per
/// request. A cold start returns `census_status: "pending"` with an empty
/// `items` and a note saying the list is UNKNOWN — never an implied "nothing to
/// clean up" — and kicks a background walk so the next request has one.
///
/// ### Four distinguishable states
///
/// `pending` (no walk yet), `unavailable` (could not compute — the reason is in
/// `census_note`), `fresh`/`stale` with items, and `fresh` with an empty list
/// and a `0` total (a MEASURED zero). A failed read and a genuinely empty
/// population never render the same.
///
/// ### Query params
///
/// * `?refresh=1` — kick a fresh walk in the BACKGROUND; the response still
///   comes from the cached snapshot with `census_refreshing: true`.
/// * `?waitSecs=N` — bounded override of the snapshot wait (capped at 10).
///   Parsed LENIENTLY: a malformed value falls back to the default rather than
///   rejecting the request. Typed as a number, axum's `Query` extractor answers
///   a bodiless 400 before this handler runs, which would contradict the "no
///   error arm" claim below — a typo in a query knob is not a reason to withhold
///   the disk report.
async fn disk_reclaimable_handler(
    State(_state): State<Arc<ApiState>>,
    Query(query): Query<DiskSurveyQuery>,
) -> Json<ApiResponse<disk_survey::DiskSurvey>> {
    // No error arm on purpose: "could not compute" is a reportable STATE
    // (`census_status: "unavailable"` with the reason in `census_note`), not a
    // 500 whose meaning a client would have to guess at. INV-D1 — the preview
    // always renders.
    Json(ApiResponse::success(disk_survey::survey(query).await))
}
