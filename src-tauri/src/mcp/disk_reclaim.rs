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
///       "roots_unknown": false,            // ⇒ the counts above ARE readings; see below
///       "by_class": [ { "class": "in-repo-canonical", "roots": 41, "bytes": …,
///                       "reclaimable_roots": 0, "reclaimable_bytes": 0,
///                       "roots_with_unknown_bytes": 0,
///                       "verb": null, "note": "…" }, … ],
///       // Free space — the 60s publisher, independent of the walk above.
///       // `drive_letter` is a LABEL, not the key, and is omitted entirely on
///       // POSIX (and on any mount that has no letter) — `volume` is the key.
///       "volumes": [ { "volume": "D:", "drive_letter": "D:",
///                      "total_bytes": 4000, "free_bytes": 93 } ],
///       "volumes_status": "fresh", "volumes_observed_at": "2026-08-18T09:14:12Z",
///       "volumes_age_secs": 12,
///       "free_bytes_total": 93, "total_bytes_total": 4000,
///       "volumes_note": "Free space as of 12s ago…" },
///     "census_status": "fresh",   // "pending" | "fresh" | "stale" | "unavailable"
///     "census_taken_at": "2026-08-18T09:11:02Z",
///     "census_age_secs": 214,
///     "census_build_ms": 88231,
///     "census_refreshing": false,
///     "census_note": "Disk state as of 3m 34s ago, from an 88231 ms walk of …",
///     // `null` — not an empty object — in `pending` and `unavailable`.
///     "scan": { "dirs_visited": 41022, "truncated": false,
///               // A capped SAMPLE; `read_errors_total` is the real count. Each
///               // entry is { "path": "D:/…/locked", "error": "Access is denied.
///               // (os error 5)" } — shown empty here only because this example
///               // is a walk that failed no read.
///               "read_errors": [], "read_errors_total": 0,
///               // The other three ways a walk can fall short of the tree.
///               // `depth_limited_dirs` is load-bearing: see the UNKNOWN
///               // population section below.
///               // `depth_limited_dirs` is the ONLY one of the four that can
///               // be non-zero beside `bytes_incomplete: false` — it is the one
///               // signal excluded from `ScanStats::incomplete()`. A non-zero
///               // `read_errors_total`, `entry_errors` or `reparse_dirs_skipped`
///               // forces `bytes_incomplete: true` and a matching clause in
///               // `census_note`, so a fixture built with one of those beside a
///               // `false` here is a payload the route never emits.
///               "entry_errors": 0, "depth_limited_dirs": 118,
///               "reparse_dirs_skipped": 0,
///               "roots_with_unknown_bytes": 0, "roots_with_partial_bytes": 0 } } }
/// ```
///
/// Every key the route serializes is above, with `drive_letter` the one field
/// that is conditionally absent (`skip_serializing_if`) rather than nullable.
/// It is a CONTRACT page, so a field missing from this block is a bug in the
/// page, not an optional field — the three walk-shortfall counters
/// (`entry_errors`, `depth_limited_dirs`, `reparse_dirs_skipped`) were on the
/// wire for two rounds of honesty fixes before they were written down here,
/// which is how a consumer came to key its completeness test on `truncated`
/// alone. That rule reaches INSIDE the collections too: a shape shown only as
/// `[]` documents nothing, which is why `read_errors`' element keys are spelled
/// out in the comment above it rather than left to be inferred from an empty
/// array.
///
/// **Five top-level keys are nullable, and the example above shows all five
/// populated.** `pending` (no walk yet) and `unavailable` (no walk possible)
/// serialize `workspace_root`, `census_taken_at`, `census_age_secs`,
/// `census_build_ms` and `scan` as `null`; `census_status`, `census_refreshing`,
/// `census_note`, `items` and `summary` are always present. Type them
/// accordingly — a consumer that read the example as the whole contract would
/// throw on the very cold-start payload this page spends four paragraphs on.
///
/// `scan` is the load-bearing one. It carries a walk, so it is `null` rather
/// than a zeroed object: a zeroed `scan` would assert a walk that visited 0
/// directories and failed 0 reads, which is the same
/// measured-shape-over-an-unknown-population defect the rest of this page is
/// about. Everything below that sends a consumer to `scan` for the CAUSE of an
/// unknown population is therefore scoped to a walk that COMPLETED; in those two
/// states the cause is `census_status` and `census_note`, and there are no
/// shortfall counters to consult.
///
/// ### Bounded by construction
///
/// Sizing every cargo target root is a multi-minute walk, so this reads the
/// snapshot published by the periodic surveyor rather than rebuilding one per
/// request. A cold start returns `census_status: "pending"` with an empty
/// `items` and a note saying the list is UNKNOWN — never an implied "nothing to
/// clean up" — and kicks a background walk so the next request has one.
///
/// ### Five distinguishable states
///
/// `pending` (no walk yet), `unavailable` (could not compute — the reason is in
/// `census_note`), `fresh`/`stale` with items, `fresh` with an empty list and a
/// `0` total (a MEASURED zero), and an empty list from a walk that did NOT see
/// the whole tree (an UNKNOWN population). A failed read and a genuinely empty
/// population never render the same.
///
/// **The last state has TWO causes, and in `summary` they render identically**
/// — which is the point, since the population is unknown either way. They
/// separate in `scan` and in `census_note`, which name the actual cause: a walk
/// that could not READ part of the tree, or a walk that stopped at its DEPTH
/// BOUND (`scan.depth_limited_dirs > 0`) without finding a single root. Any UI
/// that explains WHY must read `scan`, not `summary`, or it will assert a cause
/// the payload does not carry. The second is the commoner one — with the
/// walk's depth bound at 4 it
/// bites on essentially any deep tree, so a `paths.workspace_root` pointed one
/// level too high finds nothing at or above the bound while nothing at all
/// failed to read. Both mean the population is unknown, so both publish the
/// unknown shape below. (A walk that FOUND roots under a bitten bound is a
/// different state: its totals are measurements, `bytes_incomplete` is `false`,
/// and the bound is narrated in `census_note` because a root below it is absent
/// from `items` rather than reported as zero.)
///
/// ### What an UNKNOWN population looks like on the wire — read this first
///
/// This is the state a consumer gets wrong, because two of its three signals
/// are shapes an empty machine also produces. `summary.roots`, `reclaimable`
/// and `blocked` are `usize` and cannot be nulled, so they read `0`; the byte
/// totals go `null`, which is the half a reader notices. The three signals that
/// actually carry the statement:
///
/// * **`summary.roots_unknown: true`** — the flag the counts cannot carry
///   themselves. **Any consumer keying a "measured zero" rendering off a count
///   MUST read this first**, and read it as a veto rather than a tiebreak.
/// * **`summary.by_class: []`** — EMPTY, not four rows at `roots: 0`. Those
///   rows are bit-for-bit what a fully-read empty machine emits, so they are
///   not published at all here. A successful empty walk keeps all four rows,
///   at `roots: 0, bytes: 0`: an empty rollup and a zeroed one mean opposite
///   things, and `.every()` over an empty list is vacuously true in most
///   languages — check the length before trusting a universal.
/// * **`summary.bytes_incomplete: true`**, with `census_note` naming the gap in
///   prose. For a COMPLETED walk the three move together by construction, so a
///   consumer that reads only one of them still cannot certify a zero — which
///   is the whole reason this flag is raised here rather than left to mean "the
///   totals below are a lower bound". There are no totals below; there is no
///   reading at all.
///
///   **The lockstep stops at `pending` and `unavailable`**, where the first two
///   signals fire and this one stays `false`. That is not a gap: the flag says
///   "the totals beside me are short", and those two states have no totals and
///   no walk that could have come up short. A consumer that renders "the walk
///   stopped early and found nothing" off this flag alone would assert a
///   truncated walk over a census that never ran. Read `census_status` first.
///   (`roots_unknown` and an empty `by_class` DO both carry across every state —
///   they are co-extensive by construction. `roots_unknown` is the one to key on
///   for the version-skew reason given just below, not because it is the only
///   one that carries.)
///
/// The empty-rollup shape arrived with a runner build; an OLDER one still
/// serves the zeroed rollup over loopback indefinitely. `roots_unknown` is the
/// signal that is safe across that skew, which is why it is the one to key on.
///
/// **Do not derive completeness from `scan.truncated` alone.** It reports one
/// of five ways a walk can fall short (`read_errors_total`, `entry_errors`,
/// `depth_limited_dirs`, `reparse_dirs_skipped` are the others), and it is
/// `false` in the commonest unknown-population state there is — the depth-bound
/// one, where nothing failed and the walk simply did not reach.
///
/// For an EMPTY item list the runner has already folded all five into
/// `roots_unknown` and `bytes_incomplete`; read those rather than re-deriving
/// the predicate from one counter. **With items PRESENT, `depth_limited_dirs`
/// is folded into neither flag, by design** — the bound bites on any deep tree,
/// so folding it in would leave `bytes_incomplete` permanently true — and it
/// then lives only in `scan` and in `census_note`. That is the state a consumer
/// sees most often, so a UI that explains WHY a walk was incomplete must read
/// `depth_limited_dirs` itself: naming only a truncated walk or an unreadable
/// subtree asserts a cause the payload does not support.
///
/// ### `scan.read_errors` is a SAMPLE
///
/// The list is capped (the walk records one entry per unreadable directory
/// across up to 200,000 of them, and the lot is serialized into every
/// response). **`scan.read_errors_total` is the count**; it is never capped,
/// and it is what `census_note` and the runner's own honesty predicates read.
/// Reading `read_errors.length` as a count reports any locked subtree as
/// exactly the cap.
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
