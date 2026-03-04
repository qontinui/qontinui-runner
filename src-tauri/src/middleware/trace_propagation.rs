//! HTTP middleware for trace context propagation.
//!
//! Extracts `X-Trace-Id` from incoming requests (or generates one)
//! and includes it in the response for cross-service correlation.

use axum::{
    body::Body,
    http::{HeaderValue, Request, Response},
    middleware::Next,
};

/// Header name for trace ID propagation.
pub const TRACE_ID_HEADER: &str = "X-Trace-Id";

/// Axum middleware that propagates trace IDs through HTTP requests.
///
/// - Extracts `X-Trace-Id` from the incoming request header
/// - Generates a new trace ID if none is present
/// - Records the trace ID on the current tracing span
/// - Includes `X-Trace-Id` in the response
pub async fn trace_propagation_middleware(request: Request<Body>, next: Next) -> Response<Body> {
    // Extract or generate trace ID
    let trace_id = request
        .headers()
        .get(TRACE_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string());

    // Record on current span
    tracing::Span::current().record("trace_id", &trace_id.as_str());

    // Process the request
    let mut response = next.run(request).await;

    // Add trace ID to response
    if let Ok(header_value) = HeaderValue::from_str(&trace_id) {
        response.headers_mut().insert(TRACE_ID_HEADER, header_value);
    }

    response
}
