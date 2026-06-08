//! Degraded-boot PostgreSQL guard (D2).
//!
//! When the runner boots with `QONTINUI_ALLOW_NO_DB=1` and the canonical PG is
//! unreachable (`main.rs` degraded path), [`crate::database::pg::pg_available`]
//! is `false`. This middleware then short-circuits requests to DB-backed routes
//! with a clean `503 {success:false, code:"DATABASE_UNAVAILABLE"}` envelope
//! instead of letting them reach a handler that would hit a pool with no live
//! backend. Once the background reconnect probe (`PgDb::spawn_reconnect_probe`)
//! re-establishes PG, `pg_available()` flips back to true and the guard becomes
//! a no-op again.
//!
//! ## Comprehensive, but fail-safe
//!
//! [`route_requires_db`] classifies a path against tables derived from a full
//! audit of every route module's `routes()` + handler bodies (which touch
//! `pg_db`/`PgDb`/`.pool`/SQL). The matching is segment-wise so it can target
//! individual routes in MIXED modules (e.g. `/sessions/*/touched-files` is DB
//! but `/sessions/*/message` is not). [`DB_EXEMPT`] lists the few DB-FREE routes
//! that live UNDER a DB prefix (e.g. `/findings/summary`) so they keep working.
//!
//! Errs toward NOT 503-ing: a route we fail to list still fails safely (its
//! handler returns its own error when the pool can't connect — connection-
//! refused is immediate on a dead `localhost:5432`, no hang), so a miss is a raw
//! 500, never a hang or a broken DB-free route. The control surface, vision
//! capture, health, and the route manifest are never matched.
//!
//! ## Cost
//!
//! The fast path (PG available — the overwhelmingly common case) is a single
//! `Relaxed` atomic load and an immediate `next.run`. Path classification only
//! runs while degraded.

use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response, Json};

use crate::mcp::types::ApiResponse;

/// Route families that are ENTIRELY DB-backed — every route at or under the
/// prefix requires PG. Matched segment-wise (so `/plans` matches `/plans` and
/// `/plans/decompose` but not `/plansomething`). `*` matches one segment.
/// Verified against the live route registrations + handler bodies.
const DB_BACKED_PREFIXES: &[&str] = &[
    // Coordinator / planning / tasks
    "/plans",
    "/coordinator",
    "/workers",
    "/tasks", // completion_reports: /tasks/{id}/report, /add-dependency
    "/task-run",
    "/task-runs",
    "/current-execution",
    "/execution-spans",
    "/traces",
    "/runs", // automation_runs + regression_api drift
    "/comparison",
    "/comparisons",
    // Checks / verification / triggers
    "/checks",
    "/check-groups",
    "/tests", // verification_tests (exempt: /tests/execute-suite)
    "/test-results",
    "/triggers", // (exempt: /triggers/status)
    "/hooks",
    // Reflection / productivity / learning
    "/productivity", // reflection module
    "/productivity-knowledge",
    "/reflection", // reflection_api
    "/reflection-fixes",
    "/development-intelligence",
    "/meta-optimizer",
    "/learning",
    "/workflow-generation",
    "/online-learning/model-routing/table",
    "/online-learning/drift-signals",
    "/online-learning/step-credits",
    "/online-learning/step-scorecard",
    "/online-learning/strategies",
    "/online-learning/experiences",
    "/generator-eval",
    "/generation-rules",
    "/generation", // unified_workflows
    "/step-type-knowledge",
    "/blind-spots",
    "/prm",
    // Memory / knowledge / decisions / observations
    "/observations",
    "/decisions",
    "/concept-summaries",
    "/memory/entity-profiles",
    "/memory/search",
    "/memory/consolidate",
    "/memory/consolidation-log",
    "/memory/mental-models",
    "/memory/decay-preview",
    "/memory/health",
    "/memory/contradictions",
    "/memory/dreamer",
    "/mcp/memory", // (exempt: /mcp/memory/tool-descriptor)
    // CRUD-style registries
    "/recordings",
    "/mcp-servers",
    "/saved-api-requests",
    "/shell-commands",
    "/skills",
    "/unified-workflows",
    "/slash-commands",
    "/scheduler/tasks", // (exempt: /scheduler/tasks/*/run)
    "/state-machine/configs",
    // Errors / analytics / security / reviews
    "/error-monitor",
    "/analytics", // token_analytics
    "/security",  // security_audit (/security/audit/*)
    "/reviews",
    // Workflow / spec / scenarios / state-discovery
    "/scenarios",
    "/state-discovery",
    "/co-occurrence",
    "/spec-check",
    "/apps", // spec_api (exempt: /apps/*/spec/health, /apps/*/spec/subscribe)
    "/hitl",
    "/restate/awakeables",
    "/trace", // trace_api (exempt: /trace/health)
    // UI-Bridge DB-backed sub-surfaces (NOT the control/vision surfaces)
    "/ui-bridge/integration",
    "/ui-bridge/analytics",
    "/ui-bridge/history",
    "/ui-bridge/graph",
    // Settings backup only (rest of /settings is file/keychain-backed)
    "/settings/backup",
    // api_surface_diff (api_surface /api-surface/scan is DB-free)
    "/api-surface/diff",
    "/api-surface/snapshot",
    "/api-surface/snapshots",
];

/// Individual DB-backed routes inside MIXED modules whose DB-free siblings share
/// a parent (so a blanket prefix would over-match). Matched as EXACT segment
/// patterns; `*` matches one path-param segment.
const DB_BACKED_EXACT: &[&str] = &[
    // sessions (MIXED with /sessions/*/message, /sessions/idle-status = DB-free)
    "/sessions/spawn",
    "/sessions/*/touched-files",
    "/sessions/*/transcript",
    "/sessions/*/promote-to-worktree",
    "/sessions/*/commit-progress",
    "/sessions/*/latest-review",
    "/sessions/*/snapshots",
    "/sessions/*/rewind",
    // misc (mostly DB-free IPC; these specific ones hit PG)
    "/status",
    "/runners",
    "/instances", // GET list only; /instances/spawn etc. are DB-free
    "/instances/purge-stale",
    "/load-config",
    "/run-workflow",
    // api_requests
    "/api-request/import-to-library",
    // completion_sources (ci-pipeline is a DB-free stub)
    "/completion-sources/github-merge",
    "/completion-sources/manual-user-fire",
    // api_spec_verify (api-specs GET is filesystem)
    "/ui-bridge/specs/verify-api",
    // state_machine (runtime/bridge routes are DB-free)
    "/state-machine/available-transitions",
    "/state-machine/save-compiled",
    "/state-machine/current",
    // scheduler (tasks covered by prefix; these two are the other DB routes)
    "/scheduler/settings",
    "/scheduler/status",
    // state_explorer (only cross-run-diff hits PG)
    "/state-explorer/cross-run-diff",
    // file_registry / file_activity (rest is in-memory / coord-proxy)
    "/file-registry/probe-conflicts",
    "/file-activity/heatmap",
    // graph_api (MIXED — these branches read PG; neighborhood/paths/etc. don't)
    "/graph/summary",
    "/graph/search",
    "/graph/cross-run-patterns",
    "/graph/workflow-versions",
    "/graph/step-provenance",
    "/graph/rule-influence",
    "/graph/ineffective-rules",
    "/graph/pipeline-events",
    "/graph/phase-stats",
    "/graph/detect-patterns",
    "/graph/similar-errors",
    "/graph/ui-failure-chain",
    "/graph/ui-fix-effectiveness",
    "/graph/skill-metrics",
    // ui_bridge control workflow status (the rest of /ui-bridge/control is DB-free)
    "/ui-bridge/control/workflow/*/status",
];

/// DB-FREE routes that fall UNDER a [`DB_BACKED_PREFIXES`] entry and must keep
/// working in degraded mode. Checked FIRST; a match here means "never 503".
/// `*` matches one path-param segment.
const DB_EXEMPT: &[&str] = &[
    "/findings/summary",           // under /findings (misc; returns empty)
    "/triggers/status",            // under /triggers
    "/tests/execute-suite",        // under /tests
    "/mcp/memory/tool-descriptor", // under /mcp/memory
    "/scheduler/tasks/*/run",      // under /scheduler/tasks
    "/trace/health",               // under /trace
    "/apps/*/spec/health",         // under /apps
    "/apps/*/spec/subscribe",      // under /apps
];

/// Segment-wise match. `pattern` and `path` are split on `/` (leading/trailing
/// slashes ignored); `*` in the pattern matches exactly one path segment.
/// `prefix = true` matches when the pattern is a segment-prefix of the path
/// (the pattern itself or any sub-path); `prefix = false` requires equal length.
fn seg_match(pattern: &str, path: &str, prefix: bool) -> bool {
    let p = pattern.trim_matches('/');
    let s = path.trim_matches('/');
    let pseg: Vec<&str> = if p.is_empty() {
        vec![]
    } else {
        p.split('/').collect()
    };
    let sseg: Vec<&str> = if s.is_empty() {
        vec![]
    } else {
        s.split('/').collect()
    };
    if prefix {
        if sseg.len() < pseg.len() {
            return false;
        }
    } else if sseg.len() != pseg.len() {
        return false;
    }
    pseg.iter()
        .zip(sseg.iter())
        .all(|(pp, ss)| *pp == "*" || pp == ss)
}

/// Whether a request path targets a DB-backed route that should 503 in degraded
/// mode. Exempt routes (DB-free siblings under a DB prefix) always return false.
pub fn route_requires_db(path: &str) -> bool {
    if DB_EXEMPT.iter().any(|e| seg_match(e, path, false)) {
        return false;
    }
    if DB_BACKED_PREFIXES.iter().any(|p| seg_match(p, path, true)) {
        return true;
    }
    DB_BACKED_EXACT.iter().any(|e| seg_match(e, path, false))
}

/// The canonical `503 database unavailable` JSON envelope.
fn db_unavailable_response(path: &str) -> Response {
    use axum::response::IntoResponse;

    let body = ApiResponse::<()>::error_with_code(
        format!(
            "database unavailable — runner booted with QONTINUI_ALLOW_NO_DB and \
             PostgreSQL is unreachable; route {path} requires the database"
        ),
        "DATABASE_UNAVAILABLE",
    );
    (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
}

/// `axum::middleware::from_fn` guard. See module docs.
pub async fn pg_degraded_guard_middleware(req: Request, next: Next) -> Response {
    // Fast path: PG is available — straight through, one atomic load.
    if crate::database::pg::pg_available() {
        return next.run(req).await;
    }

    // Degraded boot: 503 the DB-backed routes; pass everything else
    // (health, control surface, vision capture, route manifest, ...).
    let path = req.uri().path();
    if route_requires_db(path) {
        return db_unavailable_response(path);
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_db_families_match_at_and_under_prefix() {
        for p in [
            "/plans",
            "/plans/decompose",
            "/coordinator/state",
            "/workers",
            "/task-runs",
            "/task-runs/abc-123/output",
            "/task-run/abc/events",
            "/checks/scan-workspace",
            "/check-groups/x/run",
            "/observations/stats",
            "/decisions/search",
            "/recordings/x/actions",
            "/reviews/recent",
            "/hooks/x/test",
            "/meta-optimizer/runs",
            "/memory/entity-profiles/search",
            "/security/audit/events",
            "/analytics/token-usage/daily",
            "/spec-check/batch",
            "/apps/myapp/spec/get",
            "/ui-bridge/integration/analyze",
            "/ui-bridge/analytics/health-score",
            "/settings/backup/export",
            "/api-surface/diff",
        ] {
            assert!(route_requires_db(p), "expected DB: {p}");
        }
    }

    #[test]
    fn mixed_module_db_routes_match_exactly() {
        for p in [
            "/sessions/spawn",
            "/sessions/abc/touched-files",
            "/sessions/abc/latest-review",
            "/status",
            "/instances",
            "/instances/purge-stale",
            "/api-request/import-to-library",
            "/completion-sources/github-merge",
            "/ui-bridge/specs/verify-api",
            "/state-machine/configs/import",
            "/state-machine/current",
            "/scheduler/settings",
            "/state-explorer/cross-run-diff",
            "/file-registry/probe-conflicts",
            "/file-activity/heatmap",
            "/graph/summary",
            "/online-learning/strategies",
            "/ui-bridge/control/workflow/run-7/status",
        ] {
            assert!(route_requires_db(p), "expected DB: {p}");
        }
    }

    #[test]
    fn exempt_and_db_free_routes_pass_through() {
        for p in [
            // Boot-critical / smoke-walked DB-free surfaces.
            "/health",
            "/ui-bridge/health",
            "/ui-bridge/status",
            "/control/elements",
            "/control/component/abc",
            "/control/element/x/expect",
            "/vision/capture",
            "/ui-bridge/_routes",
            "/ui-bridge/capabilities",
            "/awas/discover",
            // DB-free siblings of DB routes (mixed modules).
            "/sessions/abc/message",
            "/sessions/idle-status",
            "/instances/spawn",
            "/instances/abc/heartbeat",
            "/api-request/test",
            "/completion-sources/ci-pipeline",
            "/api-surface/scan",
            "/memory/invalidate-graph-cache",
            "/graph/neighborhood",
            "/online-learning/model-routing",
            "/scheduler/reconcile-now",
            "/state-machine/execute-transition",
            // Explicit exemptions under a DB prefix.
            "/findings/summary",
            "/triggers/status",
            "/tests/execute-suite",
            "/mcp/memory/tool-descriptor",
            "/scheduler/tasks/abc/run",
            "/trace/health",
            "/apps/myapp/spec/health",
            "/apps/myapp/spec/subscribe",
            // Segment-boundary lookalikes must NOT match.
            "/plansomething",
            "/task-runs-archive",
            "/reviewsx",
        ] {
            assert!(!route_requires_db(p), "expected DB-free: {p}");
        }
    }

    #[test]
    fn segment_boundary_not_substring() {
        // /productivity must not catch /productivity-knowledge (different segment).
        assert!(route_requires_db("/productivity/reflection/p1"));
        assert!(route_requires_db("/productivity-knowledge/search")); // its own prefix
                                                                      // A lookalike that is neither prefix:
        assert!(!route_requires_db("/productivityx"));
        // /trace vs /traces (distinct segments).
        assert!(route_requires_db("/traces/abc")); // task_runs /traces prefix
        assert!(route_requires_db("/trace/list")); // trace_api /trace prefix
        assert!(!route_requires_db("/trace/health")); // exempt
    }
}
