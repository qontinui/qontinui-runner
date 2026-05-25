//! Debug-only assertion layer: every 4xx/5xx response MUST be `application/json`.
//!
//! ## Purpose
//!
//! After the global [`envelope_rewrite_middleware`] rewrites `text/plain` 4xx
//! rejections into the canonical JSON envelope, this layer verifies that the
//! rewrite actually happened. If any route produces a 4xx or 5xx response with
//! a non-JSON `Content-Type`, it means a handler bypassed the envelope — either
//! by returning a raw `text/plain` body directly, or by a gap in the rewrite
//! middleware's coverage.
//!
//! ## Gate: `#[cfg(debug_assertions)]`
//!
//! This module and its middleware compile away entirely in release builds. The
//! associated `CatchPanicLayer` wraps the audit layer in running binaries, so a
//! panic in the audit layer surfaces as a 500 JSON response (not a process
//! crash) in debug-mode server runs. In tests (which also build under
//! `debug_assertions`) the panic surfaces the violating route name directly.
//!
//! ## Layer ordering (outer → inner)
//!
//! ```text
//! [CatchPanicLayer]               ← outermost: panics → 500 JSON
//!   [envelope_audit_middleware]   ← THIS LAYER (debug builds only)
//!     [envelope_rewrite_middleware] ← rewrites 4xx text/plain → JSON
//!       [TraceLayer, CORS, BodyLimit, ...]
//!         [handlers]
//! ```
//!
//! The audit layer sits INSIDE `CatchPanicLayer` (so panics are caught) and
//! OUTSIDE `envelope_rewrite` (so it observes the POST-rewrite response). A
//! non-JSON error response after the rewrite middleware is a definitive bug.
//!
//! [`envelope_rewrite_middleware`]: crate::mcp::envelope::envelope_rewrite_middleware

use axum::{
    body::Body,
    extract::Request,
    http::{header, Method, Uri},
    middleware::Next,
    response::Response,
};

/// Debug-only middleware that panics when a 4xx or 5xx response is not JSON.
///
/// Runs AFTER [`crate::mcp::envelope::envelope_rewrite_middleware`] in the
/// layer chain (i.e., observes the post-rewrite response). If the response
/// has an error status but its `Content-Type` is not `application/json`, a
/// handler bypassed the envelope — this is surfaced as a panic so CI fails
/// immediately rather than silently shipping a broken response shape.
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

    panic!(
        "[envelope_audit] {} {} returned {} with non-JSON Content-Type {:?} — \
        handler bypassed the envelope (expected application/json after rewrite middleware). \
        Fix the handler to return Json<ApiResponse<..>> or ensure envelope_rewrite_middleware \
        covers this path.",
        method,
        uri,
        status.as_u16(),
        if content_type.is_empty() {
            "(missing)".to_string()
        } else {
            content_type
        },
    );
}
