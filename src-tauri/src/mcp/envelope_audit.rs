//! Debug-only assertion layer: every 4xx/5xx response MUST be `application/json`.
//!
//! ## Purpose
//!
//! After the global [`envelope_rewrite_middleware`] rewrites non-JSON 4xx/5xx
//! responses into the canonical JSON envelope, this layer verifies that the
//! rewrite actually happened. If any route produces a 4xx or 5xx response with
//! a non-JSON `Content-Type`, it means a handler bypassed the envelope — either
//! by returning a raw `text/plain` body directly, or by a gap in the rewrite
//! middleware's coverage.
//!
//! ## Gate: `#[cfg(debug_assertions)]`
//!
//! This module and its middleware compile away entirely in release builds.
//!
//! ## The audit does NOT panic (2026-08-05)
//!
//! It used to `panic!`. That made the diagnostic more destructive than the
//! defect it reported, for three reasons — all observed live on
//! 2026-08-05T15:28:10, when a `GET /task-runs` 500 `text/plain` tripped it:
//!
//! 1. **It destroyed the evidence it was reporting on.** The panic unwound the
//!    response, so `CatchPanicLayer` replaced the handler's real 500 body (the
//!    Postgres error string from `get_recent_task_runs_filtered`) with a
//!    synthetic `handler panicked: [envelope_audit] …`. The audit's own message
//!    survived; the actual root cause did not.
//! 2. **It forged crash forensics.** The panic HOOK runs before unwinding and
//!    cannot know the panic is about to be caught, so every trip wrote a
//!    `crash_*.txt`, a `latest_crash.txt`, and a `runner-panic.log` entry, plus
//!    captured a full backtrace. `CatchPanicLayer` retracts the `crash_*.txt`
//!    afterwards, but a human or agent reading `latest_crash.txt` sees an
//!    apparent process death for a runner that never missed a request.
//! 3. **It was unassertable anyway.** `CatchPanicLayer` swallows the panic, so
//!    the "CI fails immediately" premise never held for any test that drives a
//!    router built by `mcp_api::build_router` — the layer the panic was
//!    supposed to protect.
//!
//! The codebase had already conceded the point once, narrowly: the coord-mcp
//! proxy in `mcp_api.rs` envelopes non-JSON upstream 502/504s specifically
//! because "a transient coord blip made a healthy runner look crashed". That
//! was a point fix for one route; this is the general one.
//!
//! What replaces the panic is strictly LOUDER, not quieter:
//!
//! * a `tracing::error!` carrying the same message, with structured fields
//!   (`method`, `uri`, `status`, `content_type`) that a log query can key on —
//!   a panic message is an opaque string;
//! * a process-global violation registry ([`violations`], [`violation_count`])
//!   that tests and `/health` can assert on directly, which a swallowed panic
//!   never permitted;
//! * the offending response passes through **unmodified**, so the caller and
//!   the logs still see the handler's real error.
//!
//! Set `QONTINUI_ENVELOPE_AUDIT_PANIC=1` to restore hard-fail behaviour for a
//! CI job that genuinely wants the process to die on a violation.
//!
//! ## Layer ordering (outer → inner)
//!
//! ```text
//! [CatchPanicLayer]               ← outermost: panics → 500 JSON
//!   [envelope_audit_middleware]   ← THIS LAYER (debug builds only)
//!     [envelope_rewrite_middleware] ← rewrites non-JSON 4xx/5xx → JSON
//!       [TraceLayer, CORS, BodyLimit, ...]
//!         [handlers]
//! ```
//!
//! The audit layer sits OUTSIDE `envelope_rewrite` so it observes the
//! POST-rewrite response. A non-JSON error response after the rewrite
//! middleware is a definitive bug.
//!
//! [`envelope_rewrite_middleware`]: crate::mcp::envelope::envelope_rewrite_middleware

use std::sync::{Mutex, OnceLock};

use axum::{
    body::Body,
    extract::Request,
    http::{header, Method, Uri},
    middleware::Next,
    response::Response,
};

/// Most recent envelope violations, newest last. Bounded so a route stuck in a
/// violating loop cannot grow this without limit.
const MAX_RECORDED_VIOLATIONS: usize = 32;

/// A single observed envelope escape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EnvelopeViolation {
    pub method: String,
    pub uri: String,
    pub status: u16,
    /// The offending `Content-Type`, or `"(missing)"` when the header was absent.
    pub content_type: String,
}

fn registry() -> &'static Mutex<(u64, Vec<EnvelopeViolation>)> {
    static REGISTRY: OnceLock<Mutex<(u64, Vec<EnvelopeViolation>)>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new((0, Vec::new())))
}

/// Total envelope violations observed since process start.
///
/// Never resets. `0` is the contract: any non-zero value is a handler bug that
/// shipped a non-JSON error body to a caller.
pub fn violation_count() -> u64 {
    registry().lock().map(|g| g.0).unwrap_or(0)
}

/// The most recent violations (at most [`MAX_RECORDED_VIOLATIONS`]), oldest first.
pub fn violations() -> Vec<EnvelopeViolation> {
    registry().lock().map(|g| g.1.clone()).unwrap_or_default()
}

/// Clear the registry. Test-support only — the production surface is
/// append-only so a violation can never be hidden by a later request.
#[cfg(test)]
pub fn reset_violations_for_test() {
    if let Ok(mut g) = registry().lock() {
        g.0 = 0;
        g.1.clear();
    }
}

fn record(violation: EnvelopeViolation) {
    if let Ok(mut g) = registry().lock() {
        g.0 = g.0.saturating_add(1);
        if g.1.len() >= MAX_RECORDED_VIOLATIONS {
            g.1.remove(0);
        }
        g.1.push(violation);
    }
}

/// Whether a violation should abort the process instead of being logged.
///
/// Opt-in via `QONTINUI_ENVELOPE_AUDIT_PANIC=1`, read once. Default is `false`:
/// a diagnostic must not be more destructive than the defect it reports.
fn panic_on_violation() -> bool {
    static PANIC: OnceLock<bool> = OnceLock::new();
    *PANIC.get_or_init(|| {
        std::env::var("QONTINUI_ENVELOPE_AUDIT_PANIC")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Human-readable violation message. Shared by the log line and the opt-in
/// panic so both phrase the defect identically.
fn violation_message(v: &EnvelopeViolation) -> String {
    format!(
        "[envelope_audit] {} {} returned {} with non-JSON Content-Type {:?} — \
        handler bypassed the envelope (expected application/json after rewrite middleware). \
        Fix the handler to return Json<ApiResponse<..>> or ensure envelope_rewrite_middleware \
        covers this path.",
        v.method, v.uri, v.status, v.content_type,
    )
}

/// Debug-only middleware that reports 4xx/5xx responses that are not JSON.
///
/// Runs AFTER [`crate::mcp::envelope::envelope_rewrite_middleware`] in the
/// layer chain (i.e., observes the post-rewrite response). If the response has
/// an error status but its `Content-Type` is not `application/json`, a handler
/// bypassed the envelope — recorded in the violation registry and logged at
/// ERROR. The response itself is forwarded **unchanged** so the caller still
/// receives the handler's real error rather than a synthetic one.
///
/// Request method and URI are captured before consuming `req` (since `next`
/// takes ownership of it).
pub async fn envelope_audit_middleware(req: Request<Body>, next: Next) -> Response {
    // Capture diagnostic info before consuming the request.
    let method: Method = req.method().clone();
    let uri: Uri = req.uri().clone();

    let response = next.run(req).await;
    let status = response.status();

    // Only inspect error responses.
    if !status.is_client_error() && !status.is_server_error() {
        return response;
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    // `application/json` (with or without charset) is the only acceptable
    // content-type for error responses after the rewrite middleware.
    if content_type.starts_with("application/json") {
        return response;
    }

    let violation = EnvelopeViolation {
        method: method.to_string(),
        uri: uri.to_string(),
        status: status.as_u16(),
        content_type: if content_type.is_empty() {
            "(missing)".to_string()
        } else {
            content_type
        },
    };

    let message = violation_message(&violation);
    record(violation.clone());

    if panic_on_violation() {
        panic!("{message}");
    }

    tracing::error!(
        envelope_violation = true,
        method = %violation.method,
        uri = %violation.uri,
        status = violation.status,
        content_type = %violation.content_type,
        "{}",
        message
    );

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, routing::get, Router};
    use tower::ServiceExt;

    fn plain_500() -> (StatusCode, String) {
        (StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string())
    }

    fn audited_router() -> Router {
        Router::new()
            .route("/plain-500", get(|| async { plain_500() }))
            .layer(axum::middleware::from_fn(envelope_audit_middleware))
    }

    /// Serializes the tests in this module.
    ///
    /// The violation registry is deliberately process-global and append-only
    /// (a production violation must never be hidden by a later request), so
    /// every test here mutates shared state. `cargo test` runs them on
    /// parallel threads, and without this guard one test's
    /// `reset_violations_for_test` clears a count another test just recorded
    /// and is about to assert on.
    static TEST_GUARD: Mutex<()> = Mutex::new(());

    /// Take [`TEST_GUARD`], recovering from poisoning.
    ///
    /// A failing `assert!` unwinds while holding the guard, which poisons it
    /// and would turn one real failure into a cascade of misleading
    /// `PoisonError`s in every sibling test. The lock only protects test
    /// sequencing, so resuming on poison is safe and keeps the FIRST failure
    /// the visible one.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// THE regression: a non-JSON 5xx must be reported without unwinding. The
    /// old implementation panicked here, which `CatchPanicLayer` converted to a
    /// synthetic 500 — destroying the handler's real error and writing bogus
    /// crash forensics for a process that never died.
    #[tokio::test]
    async fn non_json_error_is_recorded_not_panicked() {
        let _g = guard();
        reset_violations_for_test();

        let response = audited_router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/plain-500")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("middleware must not unwind");

        // The handler's own response survives intact.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(&body[..], b"boom", "handler's real error must be preserved");

        // …and the violation is machine-readable, which a swallowed panic never was.
        assert_eq!(violation_count(), 1);
        let v = violations();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].status, 500);
        assert_eq!(v[0].method, "GET");
        assert_eq!(v[0].uri, "/plain-500");
        assert!(
            v[0].content_type.starts_with("text/plain"),
            "expected text/plain, got {:?}",
            v[0].content_type
        );
    }

    /// A JSON error response is the contract being enforced — it must not be
    /// recorded, and must pass through untouched.
    #[tokio::test]
    async fn json_error_is_not_a_violation() {
        let _g = guard();
        reset_violations_for_test();

        let router = Router::new()
            .route(
                "/json-500",
                get(|| async {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(serde_json::json!({"success": false})),
                    )
                }),
            )
            .layer(axum::middleware::from_fn(envelope_audit_middleware));

        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/json-500")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(violation_count(), 0);
    }

    /// 2xx responses are never inspected, whatever their Content-Type.
    #[tokio::test]
    async fn success_response_is_not_inspected() {
        let _g = guard();
        reset_violations_for_test();

        let router = Router::new()
            .route("/ok", get(|| async { "plain text success" }))
            .layer(axum::middleware::from_fn(envelope_audit_middleware));

        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/ok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(violation_count(), 0);
    }

    /// The registry is bounded: a route stuck in a violating loop must not grow
    /// it without limit, but the total count keeps climbing.
    #[tokio::test]
    async fn violation_registry_is_bounded_but_count_is_not() {
        let _g = guard();
        reset_violations_for_test();

        let router = audited_router();
        for _ in 0..(MAX_RECORDED_VIOLATIONS + 5) {
            let _ = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/plain-500")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        assert_eq!(violation_count() as usize, MAX_RECORDED_VIOLATIONS + 5);
        assert_eq!(violations().len(), MAX_RECORDED_VIOLATIONS);
    }
}
