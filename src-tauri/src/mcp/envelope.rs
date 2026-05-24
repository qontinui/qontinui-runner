//! Canonical error-envelope layer for axum JSON rejections (Phase A1).
//!
//! ## Two surfaces
//!
//! * **`envelope_rewrite_middleware`** — global `axum::middleware::from_fn` that
//!   intercepts any `4xx text/plain` response produced by axum's built-in
//!   extractors and rewrites it as a `application/json` [`ApiResponse`] with a
//!   machine-readable `code`. This is the catch-all: all existing handlers get
//!   correct envelopes without any per-handler migration.
//!
//! * **`UiBridgeJson<T>`** — an opt-in `FromRequest` extractor that wraps
//!   `axum::Json<T>` and maps each [`JsonRejection`] variant to the correct
//!   envelope at extraction time, giving richer per-handler control. Later
//!   phases can migrate individual handlers to use this extractor.
//!
//! ## Ordering guarantee
//!
//! The middleware is layered BELOW the panic catcher in `mcp_api.rs` so that:
//!
//! ```text
//! [CatchPanicLayer]           <- outermost: converts panics → 500 JSON
//!   [envelope_rewrite_middleware]  <- rewrites 4xx text/plain → JSON envelope
//!     [TraceLayer, CORS, ...]
//!       [handlers]
//! ```
//!
//! Async-graphql emits `application/json` for its own errors, so the
//! `text/plain` guard ensures GraphQL responses are never rewritten.

use axum::extract::rejection::JsonRejection;
use axum::{
    body::Body,
    extract::{FromRequest, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::de::DeserializeOwned;

use crate::mcp::types::ApiResponse;

// ─── Code constants ──────────────────────────────────────────────────────────

const CODE_UNSUPPORTED_MEDIA_TYPE: &str = "UNSUPPORTED_MEDIA_TYPE";
const CODE_INVALID_JSON: &str = "INVALID_JSON";
const CODE_INVALID_REQUEST: &str = "INVALID_REQUEST";
const CODE_PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
const CODE_BAD_REQUEST: &str = "BAD_REQUEST";

// ─── Envelope helpers ─────────────────────────────────────────────────────────

/// Build a 415 envelope for a missing / wrong `Content-Type` header.
pub fn envelope_415() -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        Json(ApiResponse::<()>::error_with_code(
            "Expected request with `Content-Type: application/json`",
            CODE_UNSUPPORTED_MEDIA_TYPE,
        )),
    )
}

/// Build a 400 envelope for a JSON syntax error.
pub fn envelope_400(msg: impl Into<String>) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiResponse::<()>::error_with_code(msg, CODE_INVALID_JSON)),
    )
}

/// Build a 413 envelope for a body-too-large rejection.
pub fn envelope_413() -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(ApiResponse::<()>::error_with_code(
            "Request body exceeds the maximum allowed size",
            CODE_PAYLOAD_TOO_LARGE,
        )),
    )
}

/// Build a 422 envelope for a JSON deserialize error (wrong type / missing field).
pub fn envelope_422(msg: impl Into<String>) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ApiResponse::<()>::error_with_code(
            msg,
            CODE_INVALID_REQUEST,
        )),
    )
}

// ─── UiBridgeJson<T> extractor ───────────────────────────────────────────────

/// Opt-in JSON extractor that maps [`JsonRejection`] variants to the canonical
/// error envelope at extraction time.
///
/// Drop-in replacement for `axum::Json<T>` on handlers that want per-handler
/// envelope control. A1 stages the type; later phases migrate individual
/// handlers.
///
/// ```rust,ignore
/// async fn my_handler(UiBridgeJson(body): UiBridgeJson<MyRequest>) -> impl IntoResponse {
///     // …
/// }
/// ```
pub struct UiBridgeJson<T>(pub T);

impl<S, T> FromRequest<S> for UiBridgeJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = (StatusCode, Json<ApiResponse<()>>);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(UiBridgeJson(value)),
            Err(rejection) => Err(map_json_rejection(rejection)),
        }
    }
}

/// Map a [`JsonRejection`] to the canonical envelope tuple.
///
/// The `JsonRejection` enum (axum 0.8) is `#[non_exhaustive]` with variants:
/// - `JsonDataError`         — syntactically valid JSON, failed to deserialize into T (→ 422)
/// - `JsonSyntaxError`       — JSON parse error (→ 400)
/// - `MissingJsonContentType`— no / wrong Content-Type header (→ 415)
/// - `BytesRejection`        — body read failure (→ 400)
fn map_json_rejection(rejection: JsonRejection) -> (StatusCode, Json<ApiResponse<()>>) {
    match rejection {
        JsonRejection::MissingJsonContentType(_) => envelope_415(),
        JsonRejection::JsonSyntaxError(e) => envelope_400(e.to_string()),
        JsonRejection::JsonDataError(e) => envelope_422(e.to_string()),
        JsonRejection::BytesRejection(e) => {
            // BytesRejection fires when the body read itself fails (connection
            // reset, body-limit exceeded before the limit layer fires, etc.).
            // Treat as 400 rather than 413 because we can't reliably
            // distinguish "body too large" at this layer — the
            // RequestBodyLimitLayer fires first and produces a distinct status.
            envelope_400(e.to_string())
        }
        // Non-exhaustive: cover future variants defensively.
        _ => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::error_with_code(
                rejection.to_string(),
                CODE_BAD_REQUEST,
            )),
        ),
    }
}

// ─── Global rewrite middleware ────────────────────────────────────────────────

/// Axum middleware that rewrites `4xx text/plain` responses into the canonical
/// JSON error envelope.
///
/// ## Safety boundary
///
/// Only `text/plain` 4xx responses are rewritten. `application/json` responses
/// (including async-graphql errors) pass through untouched, as do 2xx/3xx/5xx
/// responses.
///
/// ## Body size cap
///
/// The original plain-text body is read up to 64 KiB. Bodies larger than that
/// (pathological) are discarded and the code-derived fallback message is used
/// instead.
pub async fn envelope_rewrite_middleware(req: Request, next: Next) -> Response {
    let response = next.run(req).await;

    let status = response.status();

    // Pass through non-4xx responses immediately.
    if !status.is_client_error() {
        return response;
    }

    // Check Content-Type; only rewrite text/plain.
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !content_type.starts_with("text/plain") {
        return response;
    }

    // Read the body (capped at 64 KiB to avoid holding large bodies in memory).
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let original_msg = String::from_utf8_lossy(&bytes);

    // Map the status code to a canonical error code and a human message.
    let (code, message): (&str, String) = match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => (
            CODE_UNSUPPORTED_MEDIA_TYPE,
            if original_msg.is_empty() {
                "Expected request with `Content-Type: application/json`".to_string()
            } else {
                original_msg.into_owned()
            },
        ),
        StatusCode::PAYLOAD_TOO_LARGE => (
            CODE_PAYLOAD_TOO_LARGE,
            if original_msg.is_empty() {
                "Request body exceeds the maximum allowed size".to_string()
            } else {
                original_msg.into_owned()
            },
        ),
        StatusCode::UNPROCESSABLE_ENTITY => (
            CODE_INVALID_REQUEST,
            if original_msg.is_empty() {
                "Failed to deserialize the JSON body into the target type".to_string()
            } else {
                original_msg.into_owned()
            },
        ),
        StatusCode::BAD_REQUEST => (
            CODE_INVALID_JSON,
            if original_msg.is_empty() {
                "Bad request".to_string()
            } else {
                original_msg.into_owned()
            },
        ),
        _ => (
            CODE_BAD_REQUEST,
            if original_msg.is_empty() {
                format!("Client error ({})", status.as_u16())
            } else {
                original_msg.into_owned()
            },
        ),
    };

    let envelope = ApiResponse::<()>::error_with_code(message, code);
    let mut response = (parts.status, Json(envelope)).into_response();

    // Preserve any non-Content-Type headers from the original response that
    // callers might depend on (e.g. Retry-After on 429).
    for (key, value) in &parts.headers {
        if key != header::CONTENT_TYPE && key != header::CONTENT_LENGTH {
            response.headers_mut().insert(key.clone(), value.clone());
        }
    }

    response
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, middleware, routing::post, Router};
    use serde::Deserialize;
    use tower::ServiceExt;

    /// Minimal request body used by the test router.
    #[derive(Debug, Deserialize)]
    struct Probe {
        value: String,
    }

    /// Handler that echoes `value` back in the success envelope.
    async fn probe_handler(axum::Json(body): axum::Json<Probe>) -> Json<ApiResponse<String>> {
        Json(ApiResponse::success(body.value))
    }

    /// Build a minimal test router: `POST /probe` with the envelope middleware.
    fn test_router() -> Router {
        Router::new()
            .route("/probe", post(probe_handler))
            .layer(middleware::from_fn(envelope_rewrite_middleware))
    }

    // ── helper ────────────────────────────────────────────────────────────────

    async fn send(
        app: Router,
        content_type: Option<&'static str>,
        body: &'static str,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder().method("POST").uri("/probe");
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        let req = builder.body(Body::from(body)).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    /// 415 UNSUPPORTED_MEDIA_TYPE — no Content-Type header.
    #[tokio::test]
    async fn no_content_type_gives_415_unsupported_media_type() {
        let (status, body) = send(test_router(), None, r#"{"value":"hi"}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "UNSUPPORTED_MEDIA_TYPE");
    }

    /// 400 INVALID_JSON — malformed JSON body.
    #[tokio::test]
    async fn bad_json_gives_400_invalid_json() {
        let (status, body) = send(test_router(), Some("application/json"), "not-valid-json{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_JSON");
    }

    /// 422 INVALID_REQUEST — syntactically valid JSON but missing required field.
    #[tokio::test]
    async fn missing_field_gives_422_invalid_request() {
        let (status, body) = send(
            test_router(),
            Some("application/json"),
            r#"{"other_field":"oops"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
    }

    /// 2xx text/plain response must pass through untouched (middleware MUST NOT
    /// rewrite non-error responses).
    #[tokio::test]
    async fn success_response_passes_through_unchanged() {
        let (status, body) = send(
            test_router(),
            Some("application/json"),
            r#"{"value":"hello"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert_eq!(body["data"], "hello");
    }

    /// An already-JSON 4xx response (e.g. from a handler returning an explicit
    /// error) must NOT be rewritten — the text/plain guard is the discriminator.
    #[tokio::test]
    async fn already_json_4xx_passes_through_unchanged() {
        /// Handler that returns an explicit JSON 400.
        async fn explicit_json_error() -> impl IntoResponse {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::error("explicit handler error")),
            )
        }

        let app = Router::new()
            .route("/json-err", post(explicit_json_error))
            .layer(middleware::from_fn(envelope_rewrite_middleware));

        let req = Request::builder()
            .method("POST")
            .uri("/json-err")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["success"], false);
        // The `code` field should NOT be present — the handler set `error` but not `code`.
        assert!(
            body["code"].is_null(),
            "already-JSON 4xx must not be rewritten"
        );
        assert_eq!(body["error"], "explicit handler error");
    }
}
