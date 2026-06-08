//! Degraded-boot PostgreSQL guard (D2 mechanism).
//!
//! When the runner boots with `QONTINUI_ALLOW_NO_DB=1` and the canonical PG is
//! unreachable (`main.rs` degraded path), [`crate::database::pg::pg_available`]
//! flips to `false`. This middleware then short-circuits requests to
//! KNOWN-DB-backed route families with a clean
//! `503 {success:false, code:"DATABASE_UNAVAILABLE"}` envelope instead of
//! letting them reach a handler that would hit a pool with no live backend.
//!
//! ## Conservative by design
//!
//! [`route_requires_db`] returns `true` only for an explicit allowlist of route
//! prefixes that unambiguously require PG. Every other path passes through
//! untouched, so this guard can NEVER turn a currently-working, DB-free route
//! into a 503. A DB-backed route that is *not yet* in the list still fails
//! safely in degraded mode — its handler returns its own error when the pool
//! cannot connect (connection-refused is immediate on a dead `localhost:5432`,
//! so there is no hang); listing it here only upgrades that raw 500 to a clean
//! 503. The list is the extension point: broaden it as the per-handler audit in
//! `plans/2026-06-07-runner-degraded-no-db-boot-and-dbless-ci.md` (D2 follow-up)
//! proceeds.
//!
//! ## Cost
//!
//! The fast path (PG available — the overwhelmingly common case) is a single
//! `Relaxed` atomic load and an immediate `next.run`. No allocation, no path
//! inspection. Zero measurable overhead in normal operation.

use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response, Json};

use crate::mcp::types::ApiResponse;

/// Route prefixes that are KNOWN to require PostgreSQL. Matched as whole path
/// segments (exact, or followed by `/`), so `/plans` and `/plans/decompose`
/// match but `/plansomething` does not.
///
/// Extension point — see the module docs. Verified against the live route
/// registrations in `mcp_api.rs::create_router`.
const DB_BACKED_PREFIXES: &[&str] = &[
    "/task-runs",
    "/plans",
    "/reviews",
    "/findings",
    "/decisions",
    "/observations",
    "/productivity-knowledge",
];

/// True iff `path` is at or under `prefix` on a segment boundary
/// (`prefix` itself, or `prefix` + `/...`).
fn path_under(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// Whether a request path targets a known DB-backed route family.
/// Conservative: defaults to `false` for anything not explicitly listed.
pub fn route_requires_db(path: &str) -> bool {
    DB_BACKED_PREFIXES
        .iter()
        .any(|prefix| path_under(path, prefix))
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

    // Degraded boot: 503 the known DB-backed families; pass everything else
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
    fn db_backed_prefixes_match_on_segment_boundary() {
        // Exact prefix and sub-paths match.
        assert!(route_requires_db("/task-runs"));
        assert!(route_requires_db("/task-runs/running"));
        assert!(route_requires_db("/task-runs/abc-123/output"));
        assert!(route_requires_db("/plans"));
        assert!(route_requires_db("/plans/decompose"));
        assert!(route_requires_db("/reviews/recent"));
        assert!(route_requires_db("/findings/xyz"));
        assert!(route_requires_db("/decisions/search"));
        assert!(route_requires_db("/observations/stats"));
        assert!(route_requires_db("/productivity-knowledge"));
    }

    #[test]
    fn non_db_and_lookalike_paths_pass_through() {
        // Boot-critical / DB-free surfaces the contract smoke depends on.
        assert!(!route_requires_db("/health"));
        assert!(!route_requires_db("/ui-bridge/health"));
        assert!(!route_requires_db("/control/elements"));
        assert!(!route_requires_db("/control/component/abc"));
        assert!(!route_requires_db("/vision/capture"));
        assert!(!route_requires_db("/ui-bridge/_routes"));
        // Lookalikes must NOT match — segment-boundary matching, not raw prefix.
        assert!(!route_requires_db("/plansomething"));
        assert!(!route_requires_db("/task-runs-archive"));
        assert!(!route_requires_db("/reviewsx"));
    }
}
