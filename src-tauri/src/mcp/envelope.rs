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

// ─── RequestHints trait ───────────────────────────────────────────────────────

/// Per-request-type recovery hints surfaced on a 422 (body shape) rejection.
///
/// Implement this trait on request structs that benefit from type-specific
/// guidance so callers can recover from a shape error without reading the
/// source. The defaults return `None` (no hints); override only the arms
/// relevant to your type.
///
/// **Do NOT add a blanket impl** — Rust's orphan + no-specialization rules
/// would prevent any specific impl from ever winning. Add an explicit
/// `impl RequestHints for T {}` (empty body = all defaults) for request
/// types that genuinely have no useful hint.
pub trait RequestHints {
    /// Human-readable recovery suggestions surfaced in `suggestions` on a
    /// shape error (e.g. missing field, wrong type).
    fn shape_error_suggestions() -> Option<Vec<String>> {
        None
    }
    /// Structured allowed-values payload surfaced in `data` on a shape error
    /// (e.g. `{"allowedModes": ["hard", "soft"]}`).
    fn shape_error_data() -> Option<serde_json::Value> {
        None
    }
}

// ─── Code constants ──────────────────────────────────────────────────────────

const CODE_UNSUPPORTED_MEDIA_TYPE: &str = "UNSUPPORTED_MEDIA_TYPE";
const CODE_INVALID_JSON: &str = "INVALID_JSON";
const CODE_INVALID_REQUEST: &str = "INVALID_REQUEST";
const CODE_PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
const CODE_BAD_REQUEST: &str = "BAD_REQUEST";
const CODE_METHOD_NOT_ALLOWED: &str = "METHOD_NOT_ALLOWED";

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
/// envelope control. The bound `T: RequestHints` allows per-request-type
/// recovery suggestions and allowed-values data to be surfaced on a 422
/// (body shape) rejection.
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
    T: DeserializeOwned + RequestHints,
{
    type Rejection = (StatusCode, Json<ApiResponse<()>>);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(UiBridgeJson(value)),
            Err(rejection) => Err(map_json_rejection_with_hints::<T>(rejection)),
        }
    }
}

/// Map a [`JsonRejection`] to the canonical envelope tuple, folding in
/// type-specific [`RequestHints`] on the 422 arm.
///
/// The `JsonRejection` enum (axum 0.8) is `#[non_exhaustive]` with variants:
/// - `JsonDataError`         — syntactically valid JSON, failed to deserialize into T (→ 422)
/// - `JsonSyntaxError`       — JSON parse error (→ 400)
/// - `MissingJsonContentType`— no / wrong Content-Type header (→ 415)
/// - `BytesRejection`        — body read failure (→ 400)
fn map_json_rejection_with_hints<T: RequestHints>(
    rejection: JsonRejection,
) -> (StatusCode, Json<ApiResponse<()>>) {
    match rejection {
        JsonRejection::MissingJsonContentType(_) => envelope_415(),
        JsonRejection::JsonSyntaxError(e) => {
            // Syntax errors (e.g. truncated JSON, bad escape) benefit from
            // shape hints too — a caller who sends `{action:"click"}` with
            // bad JSON likely still wants the allowed-values list.
            envelope_400_with_hints::<T>(e.to_string())
        }
        JsonRejection::JsonDataError(e) => {
            // Primary target for hints: the body was valid JSON but didn't
            // match the expected shape (wrong field type, missing required
            // field, unknown tag value).
            envelope_422_with_hints::<T>(e.to_string())
        }
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

/// Build a 400 envelope optionally enriched with type-level hints.
fn envelope_400_with_hints<T: RequestHints>(
    msg: impl Into<String>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let msg = msg.into();
    match T::shape_error_suggestions() {
        Some(suggestions) if !suggestions.is_empty() => {
            let mut resp = ApiResponse::<()>::error_with_code_and_suggestions(
                &msg,
                CODE_INVALID_JSON,
                suggestions,
            );
            resp.data = T::shape_error_data().map(|_| ());
            // Can't attach arbitrary data to `ApiResponse<()>` — surface the
            // allowed-values payload in the `hint` field instead so no info
            // is lost (hint is a `serde_json::Value`, not typed by T).
            let mut base_resp = ApiResponse::<()>::error_with_code_and_suggestions(
                msg,
                CODE_INVALID_JSON,
                T::shape_error_suggestions().unwrap_or_default(),
            );
            base_resp.hint = T::shape_error_data();
            (StatusCode::BAD_REQUEST, Json(base_resp))
        }
        _ => envelope_400(msg),
    }
}

/// Build a 422 envelope enriched with type-level hints (suggestions + data).
fn envelope_422_with_hints<T: RequestHints>(
    msg: impl Into<String>,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let msg = msg.into();
    let suggestions = T::shape_error_suggestions();
    let data = T::shape_error_data();

    match (suggestions, data) {
        (None, None) => envelope_422(msg),
        (suggestions, data) => {
            let mut resp = match suggestions {
                Some(s) if !s.is_empty() => {
                    ApiResponse::<()>::error_with_code_and_suggestions(msg, CODE_INVALID_REQUEST, s)
                }
                _ => ApiResponse::<()>::error_with_code(msg, CODE_INVALID_REQUEST),
            };
            // Surface the allowed-values payload in `hint` (a
            // `serde_json::Value` free field) so structured data like
            // `{"allowedModes":["hard","soft"]}` reaches the caller.
            resp.hint = data;
            (StatusCode::UNPROCESSABLE_ENTITY, Json(resp))
        }
    }
}

// ─── Global rewrite middleware ────────────────────────────────────────────────

/// Axum middleware that rewrites non-JSON `4xx` responses into the canonical
/// JSON error envelope.
///
/// ## Safety boundary
///
/// `4xx` responses with a `text/plain` Content-Type OR with an empty/missing
/// Content-Type are rewritten. The empty/missing case is what axum's built-in
/// method router emits for a **405 Method Not Allowed** (e.g. a `GET` against a
/// POST-only route like `/ui-bridge/control/page/read-value`): a bare response
/// with an `Allow` header and no body or Content-Type. Without this, such 405s
/// escaped the rewrite and tripped the debug-only `envelope_audit` layer,
/// surfacing as a panic / non-JSON 405 instead of a clean error envelope.
///
/// `application/json` responses (including async-graphql errors and handlers
/// that already return `Json<ApiResponse<..>>`) pass through untouched, as do
/// 2xx/3xx/5xx responses.
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

    // Check Content-Type. Rewrite `text/plain` (axum extractor rejections) and
    // also empty/missing Content-Type (axum's 405 Method-Not-Allowed and other
    // bare router rejections produce no body and no Content-Type). Anything that
    // already declares `application/json` is left untouched so we never
    // double-wrap an existing envelope.
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let should_rewrite = content_type.is_empty() || content_type.starts_with("text/plain");
    if !should_rewrite {
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
        StatusCode::METHOD_NOT_ALLOWED => (
            CODE_METHOD_NOT_ALLOWED,
            if original_msg.is_empty() {
                "HTTP method not allowed for this route (see the `Allow` response header for the supported method(s))".to_string()
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

    impl super::RequestHints for Probe {}

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

    /// 405 METHOD_NOT_ALLOWED — a wrong-method request against a method-scoped
    /// route. Axum's built-in method router emits a bare 405 with an `Allow`
    /// header and NO body / Content-Type. The rewrite middleware must turn this
    /// into a clean JSON error envelope (regression guard for the
    /// `read-value` panic: `GET /control/page/read-value` on a POST-only route
    /// previously escaped the rewrite and tripped the envelope_audit panic).
    #[tokio::test]
    async fn wrong_method_gives_405_json_envelope() {
        // `/probe` is POST-only; hit it with GET to trigger axum's 405.
        let req = Request::builder()
            .method("GET")
            .uri("/probe")
            .body(Body::empty())
            .unwrap();
        let resp = test_router().oneshot(req).await.unwrap();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            content_type.starts_with("application/json"),
            "405 must be rewritten to JSON, got Content-Type {content_type:?}"
        );
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "METHOD_NOT_ALLOWED");
    }

    // ── Phase A3 tests: per-type hints on 422 ────────────────────────────────

    /// Helper: build a UiBridgeJson<T> router + send a malformed body.
    async fn send_uib<T>(body: &'static str) -> (StatusCode, serde_json::Value)
    where
        T: serde::de::DeserializeOwned + RequestHints + Send + 'static,
    {
        async fn handler<T: serde::de::DeserializeOwned + RequestHints + Send>(
            UiBridgeJson(_): UiBridgeJson<T>,
        ) -> impl IntoResponse {
            StatusCode::OK
        }

        let app = Router::new().route("/test", axum::routing::post(handler::<T>));
        let req = Request::builder()
            .method("POST")
            .uri("/test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    // ── PageNavigateRequest: allowedModes in hint + INVALID_REQUEST code ──────

    #[derive(Debug, serde::Deserialize)]
    struct PageNavigateRequestStub {
        #[allow(dead_code)]
        url: String,
        #[serde(default)]
        #[allow(dead_code)]
        mode: Option<String>,
    }

    impl RequestHints for PageNavigateRequestStub {
        fn shape_error_suggestions() -> Option<Vec<String>> {
            Some(vec![
                "Required field: `url` (string). Optional: `mode` (\"hard\" | \"soft\")."
                    .to_string(),
            ])
        }
        fn shape_error_data() -> Option<serde_json::Value> {
            Some(serde_json::json!({ "allowedModes": ["hard", "soft"] }))
        }
    }

    /// 422 with `allowedModes` in `hint` and `INVALID_REQUEST` code.
    #[tokio::test]
    async fn page_navigate_request_422_carries_allowed_modes_hint() {
        // Missing required `url` field → JsonDataError → 422
        let (status, body) = send_uib::<PageNavigateRequestStub>(r#"{"mode": "hard"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(
            body["code"], "INVALID_REQUEST",
            "code must be INVALID_REQUEST on shape error"
        );
        let suggestions = body["suggestions"].as_array().expect("suggestions array");
        assert!(!suggestions.is_empty(), "suggestions must not be empty");
        let hint = &body["hint"];
        assert!(
            !hint.is_null(),
            "hint must be populated with allowedModes for navigate"
        );
        let modes = hint["allowedModes"].as_array().expect("allowedModes array");
        assert!(
            modes.iter().any(|m| m.as_str() == Some("hard")),
            "allowedModes must contain 'hard'"
        );
        assert!(
            modes.iter().any(|m| m.as_str() == Some("soft")),
            "allowedModes must contain 'soft'"
        );
    }

    // ── AssertRequestStub: allowedAssertionTypes in hint ─────────────────────

    #[derive(Debug, serde::Deserialize)]
    struct AssertRequestStub {
        #[allow(dead_code)]
        assertions: Vec<serde_json::Value>,
    }

    impl RequestHints for AssertRequestStub {
        fn shape_error_suggestions() -> Option<Vec<String>> {
            Some(vec![
                "Required field: `assertions` (array of Assertion objects).".to_string(),
                "Assertion `type` values: no_overlap, contains_text, text_fits_container, \
                 aligned_horizontally, aligned_vertically, color_within, \
                 typography_consistent, no_layout_shift_since, no_clipping, \
                 animation_settled, contrast_meets_wcag."
                    .to_string(),
            ])
        }
        fn shape_error_data() -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "allowedAssertionTypes": [
                    "no_overlap", "contains_text", "text_fits_container",
                    "aligned_horizontally", "aligned_vertically", "color_within",
                    "typography_consistent", "no_layout_shift_since", "no_clipping",
                    "animation_settled", "contrast_meets_wcag"
                ]
            }))
        }
    }

    /// 422 with `allowedAssertionTypes` in `hint`.
    #[tokio::test]
    async fn assert_request_422_carries_assertion_types_hint() {
        // Missing required `assertions` field → 422
        let (status, body) = send_uib::<AssertRequestStub>(r#"{"target": "foo"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
        let hint = &body["hint"];
        assert!(!hint.is_null(), "hint must carry assertion DSL info");
        let types = hint["allowedAssertionTypes"]
            .as_array()
            .expect("allowedAssertionTypes array");
        assert!(
            types.iter().any(|t| t.as_str() == Some("no_overlap")),
            "no_overlap must be in allowed types"
        );
        assert!(
            types
                .iter()
                .any(|t| t.as_str() == Some("contrast_meets_wcag")),
            "contrast_meets_wcag must be in allowed types"
        );
    }

    // ── AnalyzeRequestStub: allowedAnalyzers in hint ──────────────────────────

    #[derive(Debug, serde::Deserialize)]
    struct AnalyzeRequestStub {
        #[allow(dead_code)]
        analyzer: String,
    }

    impl RequestHints for AnalyzeRequestStub {
        fn shape_error_suggestions() -> Option<Vec<String>> {
            Some(vec![
                "Required field: `analyzer` (one of: \"layout\", \"typography\", \
                 \"color\", \"dynamic\", \"elements\")."
                    .to_string(),
            ])
        }
        fn shape_error_data() -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "allowedAnalyzers": ["layout", "typography", "color", "dynamic", "elements"]
            }))
        }
    }

    /// 422 with `allowedAnalyzers` in `hint`.
    #[tokio::test]
    async fn analyze_request_422_carries_allowed_analyzers_hint() {
        // Missing required `analyzer` field → 422
        let (status, body) = send_uib::<AnalyzeRequestStub>(r#"{"target": "foo"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
        let hint = &body["hint"];
        assert!(!hint.is_null(), "hint must carry analyzer info");
        let analyzers = hint["allowedAnalyzers"]
            .as_array()
            .expect("allowedAnalyzers");
        assert_eq!(analyzers.len(), 5, "should expose all 5 analyzers");
        assert!(
            analyzers.iter().any(|a| a.as_str() == Some("layout")),
            "layout must be listed"
        );
    }

    // ── NavigateAndWaitRequestStub: allowedActions in hint ────────────────────

    #[derive(Debug, serde::Deserialize)]
    struct NavigateAndWaitRequestStub {
        #[allow(dead_code)]
        element_id: String,
    }

    impl RequestHints for NavigateAndWaitRequestStub {
        fn shape_error_suggestions() -> Option<Vec<String>> {
            Some(vec!["Required field: `elementId` (string).".to_string()])
        }
        fn shape_error_data() -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "allowedActions": ["click", "doubleClick", "hover", "type"]
            }))
        }
    }

    /// 422 with `allowedActions` in `hint`.
    #[tokio::test]
    async fn navigate_and_wait_request_422_carries_allowed_actions_hint() {
        // Missing required `elementId` field → 422
        let (status, body) = send_uib::<NavigateAndWaitRequestStub>(r#"{"action": "click"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
        let hint = &body["hint"];
        assert!(!hint.is_null(), "hint must carry allowedActions");
        let actions = hint["allowedActions"].as_array().expect("allowedActions");
        assert!(
            actions.iter().any(|a| a.as_str() == Some("click")),
            "click must be in allowedActions"
        );
    }

    // ── Hint-less request: no suggestions or hint fields ─────────────────────

    #[derive(Debug, serde::Deserialize)]
    struct NakedRequest {
        #[allow(dead_code)]
        value: String,
    }

    impl RequestHints for NakedRequest {}

    /// A type with no hints produces the baseline 422 envelope without
    /// `suggestions` or `hint` fields.
    #[tokio::test]
    async fn naked_request_422_has_no_hint_or_suggestions() {
        let (status, body) = send_uib::<NakedRequest>(r#"{"other": "oops"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INVALID_REQUEST");
        // No hints provided — these fields must be absent (null in serde_json::Value terms).
        assert!(body["suggestions"].is_null(), "suggestions must be absent");
        assert!(body["hint"].is_null(), "hint must be absent");
    }
}
