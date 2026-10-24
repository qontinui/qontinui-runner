//! PostgreSQL availability guard.
//!
//! [`crate::database::pg::pg_available`] is `false` while the global pool's
//! circuit breaker is not Closed (plan
//! `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`): on a
//! DEGRADED BOOT -- a configured external PG was unreachable and
//! `QONTINUI_ALLOW_NO_DB=1` opted into degrading instead of panicking, or the
//! BUNDLED cluster failed to start or connect -- AND at RUNTIME, from the first
//! connect-class failure until the breaker's background prober reconnects.
//! This middleware then short-circuits requests to DB-backed routes with a
//! `503 {success:false, code:"DATABASE_UNAVAILABLE"}` envelope that names the
//! breaker's own facts (`cause`, `causeClass`, `since`, `retryAfterMs`,
//! `lastError`, plus a `Retry-After` header) instead of letting them reach a
//! handler. Once the prober reconnects, `pg_available()` flips back to true and
//! the guard is a no-op again.
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
//! Errs toward NOT 503-ing: a route we fail to list still fails safely, so a
//! miss is a raw 500, never a hang or a broken DB-free route. Its handler gets
//! the pool's own immediate `DATABASE_UNAVAILABLE` error while the breaker is
//! open. (This used to read "connection-refused is immediate on a dead
//! `localhost:5432`, no hang". That is false on Windows, where a connect to a
//! closed loopback port costs ~2 s of SYN retries per resolved address; the
//! breaker, not the OS, is what makes the miss cheap.) The control surface,
//! vision capture, health, and the route manifest are never matched.
//!
//! ## Cost
//!
//! The fast path (PG available — the overwhelmingly common case) is a single
//! `Relaxed` atomic load and an immediate `next.run`. Path classification only
//! runs while degraded.

use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response, Json};

use crate::mcp::types::ApiResponse;
use qontinui_runner_lib::pg_circuit::DbUnavailable;

/// Route families that are ENTIRELY DB-backed — every route at or under the
/// prefix requires PG. Matched segment-wise (so `/workers` matches `/workers`
/// and `/workers/x/status` but not `/workersomething`). `*` matches one
/// segment.
/// Verified against the live route registrations + handler bodies.
const DB_BACKED_PREFIXES: &[&str] = &[
    // task runs
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
    "/apps", // spec_api + journey (exempt: /apps/*/spec/health, /apps/*/spec/subscribe, /apps/*/journey/health)
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

/// Routes (DB-free, or degrading on their own) that fall UNDER a
/// [`DB_BACKED_PREFIXES`] entry and must keep
/// working in degraded mode. Checked FIRST; a match here means "never 503".
/// `*` matches one path-param segment.
const DB_EXEMPT: &[&str] = &[
    "/findings/summary",           // under /findings (misc; returns empty)
    "/triggers/status",            // under /triggers; reads PG but degrades to null itself
    "/tests/execute-suite",        // under /tests
    "/mcp/memory/tool-descriptor", // under /mcp/memory
    "/scheduler/tasks/*/run",      // under /scheduler/tasks
    "/trace/health",               // under /trace
    "/apps/*/spec/health",         // under /apps
    "/apps/*/spec/subscribe",      // under /apps
    "/apps/*/journey/health",      // under /apps; answers from in-process counters
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

/// The human-readable reason, worded by what the breaker actually knows: a
/// degraded boot, a refused connect, or a connect that merely TIMED OUT (which
/// is not evidence the database is down, so it never says "unreachable").
fn unavailable_message(path: &str, facts: Option<&DbUnavailable>) -> String {
    let reason = facts
        .map(DbUnavailable::reason)
        .unwrap_or_else(|| "PostgreSQL is not available".to_string());
    format!("database unavailable — {reason}; route {path} requires the database")
}

/// The canonical `503 database unavailable` JSON envelope: the ApiResponse
/// error shape plus the breaker's facts, and a `Retry-After` header (whole
/// seconds, rounded up) when the next reconnect attempt is scheduled.
fn db_unavailable_response(path: &str, facts: Option<DbUnavailable>) -> Response {
    use axum::response::IntoResponse;

    let body = ApiResponse::<()>::error_with_code(
        unavailable_message(path, facts.as_ref()),
        "DATABASE_UNAVAILABLE",
    );
    let mut json = serde_json::to_value(&body).unwrap_or_else(|_| serde_json::json!({}));
    let retry_after = facts.as_ref().and_then(|f| f.retry_after);
    if let (Some(obj), Some(f)) = (json.as_object_mut(), facts.as_ref()) {
        obj.insert("cause".into(), serde_json::json!(f.cause));
        obj.insert(
            "causeClass".into(),
            serde_json::json!(f.cause_class.as_str()),
        );
        obj.insert(
            "since".into(),
            serde_json::json!(f.since.map(qontinui_runner_lib::pg_circuit::rfc3339)),
        );
        obj.insert(
            "retryAfterMs".into(),
            serde_json::json!(f.retry_after.map(|d| d.as_millis() as u64)),
        );
        obj.insert("lastError".into(), serde_json::json!(f.last_error));
    }
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, Json(json)).into_response();
    if let Some(d) = retry_after {
        let secs = d.as_millis().div_ceil(1000).max(1) as u64;
        if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, v);
        }
    }
    response
}

/// The handler-side 503 for an outage a handler met itself (the breaker
/// tripped mid-request, or the route is not in the guard's tables): the
/// same status and `code` the guard serves, carrying the error string.
pub fn db_unavailable_error(message: String) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiResponse::error_with_code(
            message,
            "DATABASE_UNAVAILABLE",
        )),
    )
}

/// `axum::middleware::from_fn` guard. See module docs.
pub async fn pg_degraded_guard_middleware(req: Request, next: Next) -> Response {
    // Fast path: the breaker is Closed — straight through, one atomic load.
    if crate::database::pg::pg_available() {
        return next.run(req).await;
    }

    // Breaker open (degraded boot or a runtime outage): 503 the DB-backed
    // routes; pass everything else
    // (health, control surface, vision capture, route manifest, ...).
    let path = req.uri().path();
    if route_requires_db(path) {
        return db_unavailable_response(path, crate::database::pg::pg_unavailable_facts());
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_db_families_match_at_and_under_prefix() {
        for p in [
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
            "/apps/myapp/journey/health",
            // Segment-boundary lookalikes must NOT match.
            "/task-runs-archive",
            "/reviewsx",
        ] {
            assert!(!route_requires_db(p), "expected DB-free: {p}");
        }
    }

    async fn body_json(r: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn facts(cause: &str, class: qontinui_runner_lib::pg_circuit::CauseClass) -> DbUnavailable {
        DbUnavailable {
            since: Some(std::time::SystemTime::UNIX_EPOCH),
            cause: cause.to_string(),
            cause_class: class,
            last_error: Some("error connecting to server".to_string()),
            retry_after: Some(std::time::Duration::from_millis(1500)),
            connect_timeout: Some(std::time::Duration::from_secs(5)),
        }
    }

    #[tokio::test]
    async fn the_503_body_carries_the_breakers_facts_and_a_retry_after_header() {
        use qontinui_runner_lib::pg_circuit::{CauseClass, CAUSE_RUNTIME};
        let r = db_unavailable_response(
            "/task-runs",
            Some(facts(CAUSE_RUNTIME, CauseClass::ConnectRefused)),
        );
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            r.headers().get(axum::http::header::RETRY_AFTER).unwrap(),
            "2",
            "1500 ms rounds up to 2 s"
        );
        let j = body_json(r).await;
        assert_eq!(j["code"], "DATABASE_UNAVAILABLE");
        assert_eq!(j["success"], false);
        assert_eq!(j["cause"], "runtime");
        assert_eq!(j["causeClass"], "connect_refused");
        assert_eq!(j["since"], "1970-01-01T00:00:00Z");
        assert_eq!(j["retryAfterMs"], 1500);
        assert_eq!(j["lastError"], "error connecting to server");
        let msg = j["error"].as_str().unwrap();
        assert!(msg.contains("unreachable"), "{msg}");
        assert!(
            !msg.contains("booted"),
            "a runtime outage is not a degraded boot: {msg}"
        );
    }

    #[tokio::test]
    async fn each_cause_is_worded_by_what_the_breaker_knows() {
        use qontinui_runner_lib::pg_circuit::{CauseClass, CAUSE_BOOT, CAUSE_RUNTIME};
        let boot = body_json(db_unavailable_response(
            "/x",
            Some(facts(CAUSE_BOOT, CauseClass::Unknown)),
        ))
        .await;
        assert!(boot["error"]
            .as_str()
            .unwrap()
            .contains("booted without its database"));
        assert_eq!(boot["cause"], "boot");
        let timeout = body_json(db_unavailable_response(
            "/x",
            Some(facts(CAUSE_RUNTIME, CauseClass::ConnectTimeout)),
        ))
        .await;
        let msg = timeout["error"].as_str().unwrap();
        assert!(msg.contains("did not answer within 5000 ms"), "{msg}");
        assert!(
            !msg.contains("unreachable"),
            "a timeout is not unreachability: {msg}"
        );
        assert_eq!(timeout["causeClass"], "connect_timeout");
        // No facts (no global PgDb): the envelope still answers, with no header.
        let r = db_unavailable_response("/x", None);
        assert!(r.headers().get(axum::http::header::RETRY_AFTER).is_none());
        let none = body_json(r).await;
        assert_eq!(none["code"], "DATABASE_UNAVAILABLE");
    }

    #[test]
    fn segment_boundary_not_substring() {
        // /productivity-knowledge is its own segment; a lookalike prefix and
        // the deleted board's /productivity family must not match.
        assert!(route_requires_db("/productivity-knowledge/search"));
        assert!(!route_requires_db("/productivity/reflection/p1"));
        assert!(!route_requires_db("/productivityx"));
        // /trace vs /traces (distinct segments).
        assert!(route_requires_db("/traces/abc")); // task_runs /traces prefix
        assert!(route_requires_db("/trace/list")); // trace_api /trace prefix
        assert!(!route_requires_db("/trace/health")); // exempt
    }
}
