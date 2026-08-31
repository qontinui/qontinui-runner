//! Canonical error-envelope layer for axum JSON rejections (Phase A1).
//!
//! ## Two surfaces
//!
//! * **`envelope_rewrite_middleware`** — global `axum::middleware::from_fn` that
//!   intercepts any `4xx`/`5xx` `text/plain` (or Content-Type-less) response —
//!   whether produced by axum's built-in extractors or by a handler's
//!   `(StatusCode, String)` error arm — and rewrites it as an
//!   `application/json` [`ApiResponse`] with a machine-readable `code`. This is
//!   the catch-all: all existing handlers get correct envelopes without any
//!   per-handler migration.
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
//!   [envelope_audit_middleware]    <- debug-only: REPORTS non-JSON errors
//!     [envelope_rewrite_middleware]  <- rewrites 4xx/5xx text/plain → JSON
//!       [TraceLayer, CORS, ...]
//!         [handlers]
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
const CODE_INTERNAL_ERROR: &str = "INTERNAL_ERROR";
const CODE_NOT_IMPLEMENTED: &str = "NOT_IMPLEMENTED";
const CODE_BAD_GATEWAY: &str = "BAD_GATEWAY";
const CODE_SERVICE_UNAVAILABLE: &str = "SERVICE_UNAVAILABLE";
const CODE_GATEWAY_TIMEOUT: &str = "GATEWAY_TIMEOUT";

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

/// Axum middleware that rewrites non-JSON `4xx` **and `5xx`** responses into
/// the canonical JSON error envelope.
///
/// ## Safety boundary
///
/// Error responses with a `text/plain` Content-Type OR with an empty/missing
/// Content-Type are rewritten. The empty/missing case is what axum's built-in
/// method router emits for a **405 Method Not Allowed** (e.g. a `GET` against a
/// POST-only route like `/ui-bridge/control/page/read-value`): a bare response
/// with an `Allow` header and no body or Content-Type. Without this, such 405s
/// escaped the rewrite and tripped the debug-only `envelope_audit` layer,
/// surfacing as a panic / non-JSON 405 instead of a clean error envelope.
///
/// `application/json` responses (including async-graphql errors and handlers
/// that already return `Json<ApiResponse<..>>`) pass through untouched, as do
/// 2xx/3xx responses and error responses declaring any other concrete
/// Content-Type (`text/html`, `text/event-stream`, …) — those are deliberate
/// non-JSON surfaces, not envelope escapes.
///
/// ## Why 5xx is covered
///
/// The overwhelmingly common handler signature in this crate is
/// `Result<Json<ApiResponse<T>>, (StatusCode, String)>` (156 occurrences across
/// 28 modules as of this change). Axum renders the `(StatusCode, String)` error
/// arm as `text/plain; charset=utf-8`, so **every** handler failure — almost
/// always a 500 — produced a bare plain-text body that escaped the envelope.
/// Restricting this middleware to `is_client_error()` meant the audit layer
/// asserted an invariant the rewrite never established: live evidence
/// 2026-08-05T15:28:10, `GET /task-runs` 500 `text/plain` from
/// `task_runs::list_task_runs`'s `.map_err(|e| (INTERNAL_SERVER_ERROR, e))`.
/// Covering 5xx here fixes the whole class at once instead of migrating 156
/// call sites.
///
/// ## Body size cap
///
/// The original plain-text body is read up to 64 KiB. Bodies larger than that
/// (pathological) are discarded and the code-derived fallback message is used
/// instead.
///
/// ## Why JSON 5xx bodies are also touched
///
/// See `stamp_json_error_code`: a handler returning `Json(api_error(..))` is
/// already `application/json`, so it used to pass straight through — with
/// `code: None`, because `api_error()` has no status to derive one from. That
/// left every 5xx built that way untyped while the 4xx around them were
/// typed. Stamping here reaches all of them at once.
pub async fn envelope_rewrite_middleware(req: Request, next: Next) -> Response {
    let response = next.run(req).await;

    let status = response.status();

    // Pass through anything that isn't an error response.
    if !status.is_client_error() && !status.is_server_error() {
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
        // Already JSON. That used to mean "already an envelope, leave it
        // alone" — but an envelope built by `api_error()` carries `code:
        // None`, so a handler returning `Json(api_error(..))` on a 500
        // produced a typed-code-less failure that this middleware skipped.
        // Measured across 105 read-only GETs: all 7 of the 4xx carried a
        // `code`, and all 4 of the 5xx carried neither `code` nor
        // `error_detail` (`/ui-bridge/analytics/health-score`,
        // `/ui-bridge/explore/{results,status}`, `/ui-bridge/cloud-devices`).
        // Stamping the status-derived code here fixes the whole 5xx class
        // centrally instead of migrating every `api_error` call site.
        if status.is_server_error() && content_type.starts_with("application/json") {
            return stamp_json_error_code(response).await;
        }
        return response;
    }

    // Read the body (capped at 64 KiB to avoid holding large bodies in memory).
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let original_msg = String::from_utf8_lossy(&bytes);

    // Map the status code to a canonical error code and a human message.
    // The code half lives in `code_for_status` so the JSON stamping pass
    // below answers `BAD_GATEWAY` for a 502 whichever shape the handler
    // happened to emit — one mapping, not two that can drift apart.
    let code = code_for_status(status);
    let message: String = if original_msg.is_empty() {
        default_message_for_status(status)
    } else {
        // ─── 5xx ──────────────────────────────────────────────────────────
        // The `(StatusCode, String)` handler-error idiom lands here: the
        // String IS the diagnostic, so it is preserved verbatim as `message`
        // and only the wire shape changes.
        original_msg.into_owned()
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

/// The canonical `code` for an HTTP error status.
///
/// Single source of truth for both consumers below (the `text/plain` rewrite
/// and the JSON code-stamping pass). The `_` arm reproduces the historical
/// behaviour exactly — any 4xx this match does not name is `BAD_REQUEST` —
/// so extending the JSON pass changed no existing code on the wire.
fn code_for_status(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => CODE_UNSUPPORTED_MEDIA_TYPE,
        StatusCode::PAYLOAD_TOO_LARGE => CODE_PAYLOAD_TOO_LARGE,
        StatusCode::UNPROCESSABLE_ENTITY => CODE_INVALID_REQUEST,
        StatusCode::BAD_REQUEST => CODE_INVALID_JSON,
        StatusCode::METHOD_NOT_ALLOWED => CODE_METHOD_NOT_ALLOWED,
        StatusCode::INTERNAL_SERVER_ERROR => CODE_INTERNAL_ERROR,
        StatusCode::NOT_IMPLEMENTED => CODE_NOT_IMPLEMENTED,
        StatusCode::BAD_GATEWAY => CODE_BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE => CODE_SERVICE_UNAVAILABLE,
        StatusCode::GATEWAY_TIMEOUT => CODE_GATEWAY_TIMEOUT,
        s if s.is_server_error() => CODE_INTERNAL_ERROR,
        _ => CODE_BAD_REQUEST,
    }
}

/// Human message used only when the original response carried no body at all.
fn default_message_for_status(status: StatusCode) -> String {
    match status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => {
            "Expected request with `Content-Type: application/json`".to_string()
        }
        StatusCode::PAYLOAD_TOO_LARGE => {
            "Request body exceeds the maximum allowed size".to_string()
        }
        StatusCode::UNPROCESSABLE_ENTITY => {
            "Failed to deserialize the JSON body into the target type".to_string()
        }
        StatusCode::BAD_REQUEST => "Bad request".to_string(),
        StatusCode::METHOD_NOT_ALLOWED => "HTTP method not allowed for this route (see the \
             `Allow` response header for the supported method(s))"
            .to_string(),
        StatusCode::INTERNAL_SERVER_ERROR => "Internal server error".to_string(),
        StatusCode::NOT_IMPLEMENTED => "Not implemented".to_string(),
        StatusCode::BAD_GATEWAY => "Bad gateway".to_string(),
        StatusCode::SERVICE_UNAVAILABLE => "Service unavailable".to_string(),
        StatusCode::GATEWAY_TIMEOUT => "Gateway timeout".to_string(),
        s if s.is_server_error() => format!("Server error ({})", s.as_u16()),
        s => format!("Client error ({})", s.as_u16()),
    }
}

/// Insert a status-derived `code` into a JSON error envelope that has none.
///
/// Returns `None` — meaning "leave the body byte-for-byte alone" — for
/// anything that is not an `ApiResponse`-shaped failure missing its `code`.
/// A body that already carries a `code` is never rewritten: a handler that
/// took the trouble to pick a precise code outranks the status-derived guess.
fn stamp_code_on_json_envelope(bytes: &[u8], status: StatusCode) -> Option<Vec<u8>> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let obj = value.as_object_mut()?;
    // Only ApiResponse failures. A 5xx whose body is some other JSON shape
    // (a GraphQL error, a proxied upstream payload) is not ours to edit.
    if obj.get("success").and_then(serde_json::Value::as_bool) != Some(false) {
        return None;
    }
    match obj.get("code") {
        None | Some(serde_json::Value::Null) => {}
        Some(_) => return None,
    }
    obj.insert(
        "code".to_string(),
        serde_json::Value::String(code_for_status(status).to_string()),
    );
    serde_json::to_vec(&value).ok()
}

/// Buffer a JSON error response and stamp a typed `code` onto it.
///
/// Only bodies whose full length is already known and small are touched.
/// Consuming a streamed body we then failed to buffer would DESTROY it, and
/// an error envelope is never large — so an absent or oversized
/// `Content-Length` passes through untouched rather than risking the body.
async fn stamp_json_error_code(response: Response) -> Response {
    const MAX_BODY: usize = 64 * 1024;

    let status = response.status();
    let declared_len = response
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok());
    if !matches!(declared_len, Some(n) if n <= MAX_BODY) {
        return response;
    }

    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        // Unreachable given the Content-Length gate above, but a consumed
        // body cannot be handed back — emit a valid envelope rather than a
        // bodyless 5xx.
        return (
            parts.status,
            Json(ApiResponse::<()>::error_with_code(
                default_message_for_status(status),
                code_for_status(status),
            )),
        )
            .into_response();
    };

    let Some(patched) = stamp_code_on_json_envelope(&bytes, status) else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };

    let mut parts = parts;
    parts.headers.remove(header::CONTENT_LENGTH);
    let patched_len = patched.len();
    let mut response = Response::from_parts(parts, axum::body::Body::from(patched));
    if let Ok(v) = header::HeaderValue::from_str(&patched_len.to_string()) {
        response.headers_mut().insert(header::CONTENT_LENGTH, v);
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

    // ── 5xx coverage (2026-08-05) ────────────────────────────────────────────

    /// Build a router whose handler uses the crate's dominant error idiom,
    /// `Result<Json<..>, (StatusCode, String)>`. Axum renders that error arm as
    /// `text/plain`, which is precisely what escaped the rewrite.
    fn tuple_error_router(status: StatusCode, msg: &'static str) -> Router {
        Router::new()
            .route(
                "/boom",
                axum::routing::get(move || async move {
                    Err::<Json<ApiResponse<()>>, (StatusCode, String)>((status, msg.to_string()))
                }),
            )
            .layer(middleware::from_fn(envelope_rewrite_middleware))
    }

    async fn get_boom(app: Router) -> (StatusCode, String, serde_json::Value) {
        let req = Request::builder()
            .method("GET")
            .uri("/boom")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
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
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, content_type, json)
    }

    /// THE regression for the 2026-08-05T15:28:10 crash dump: `GET /task-runs`
    /// returned a 500 as `text/plain` because `envelope_rewrite_middleware`
    /// short-circuited on `!status.is_client_error()`, so no 5xx was ever
    /// enveloped. The audit layer then reported a violation the rewrite had
    /// never promised to prevent.
    #[tokio::test]
    async fn plain_text_500_is_rewritten_to_json_envelope() {
        let (status, content_type, body) = get_boom(tuple_error_router(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Database error: connection refused",
        ))
        .await;

        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "status preserved"
        );
        assert!(
            content_type.starts_with("application/json"),
            "500 must be rewritten to JSON, got Content-Type {content_type:?}"
        );
        assert_eq!(body["success"], false);
        assert_eq!(body["code"], "INTERNAL_ERROR");
        assert_eq!(
            body["error"], "Database error: connection refused",
            "the handler's diagnostic string must survive the rewrite verbatim"
        );
    }

    /// Each mapped 5xx keeps its status and gets its own machine-readable code.
    #[tokio::test]
    async fn mapped_5xx_statuses_get_distinct_codes() {
        for (status, expected_code) in [
            (StatusCode::NOT_IMPLEMENTED, "NOT_IMPLEMENTED"),
            (StatusCode::BAD_GATEWAY, "BAD_GATEWAY"),
            (StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE"),
            (StatusCode::GATEWAY_TIMEOUT, "GATEWAY_TIMEOUT"),
        ] {
            let (got, content_type, body) = get_boom(tuple_error_router(status, "upstream")).await;
            assert_eq!(got, status, "status must be preserved");
            assert!(content_type.starts_with("application/json"));
            assert_eq!(body["code"], expected_code, "for status {status}");
        }
    }

    /// An unmapped 5xx (e.g. 507) still gets enveloped under INTERNAL_ERROR
    /// rather than falling into the old client-error fallback.
    #[tokio::test]
    async fn unmapped_5xx_falls_back_to_internal_error() {
        let (status, content_type, body) =
            get_boom(tuple_error_router(StatusCode::INSUFFICIENT_STORAGE, "")).await;
        assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
        assert!(content_type.starts_with("application/json"));
        assert_eq!(body["code"], "INTERNAL_ERROR");
        assert_eq!(body["error"], "Server error (507)");
    }

    /// A handler that already returns a JSON 5xx must not be double-wrapped.
    #[tokio::test]
    async fn already_json_5xx_passes_through_unchanged() {
        async fn explicit_json_500() -> impl IntoResponse {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::error("explicit handler 500")),
            )
        }

        let app = Router::new()
            .route("/json-500", axum::routing::get(explicit_json_500))
            .layer(middleware::from_fn(envelope_rewrite_middleware));

        let req = Request::builder()
            .method("GET")
            .uri("/json-500")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            body["code"].is_null(),
            "already-JSON 5xx must not be rewritten"
        );
        assert_eq!(body["error"], "explicit handler 500");
    }

    /// Error responses declaring a concrete non-plain Content-Type (SSE, HTML)
    /// are deliberate surfaces, not envelope escapes — they must pass through.
    #[tokio::test]
    async fn non_plain_content_type_5xx_is_not_rewritten() {
        async fn html_500() -> impl IntoResponse {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                "<html>oops</html>",
            )
        }

        let app = Router::new()
            .route("/html-500", axum::routing::get(html_500))
            .layer(middleware::from_fn(envelope_rewrite_middleware));

        let req = Request::builder()
            .method("GET")
            .uri("/html-500")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8"),
        );
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
                "Assertion `type` values: no_overlap, element_above, contains_text, \
                 text_fits_container, aligned_horizontally, aligned_vertically, \
                 color_within, typography_consistent, no_layout_shift_since, \
                 no_clipping, animation_settled, contrast_meets_wcag."
                    .to_string(),
            ])
        }
        fn shape_error_data() -> Option<serde_json::Value> {
            Some(serde_json::json!({
                "allowedAssertionTypes": [
                    "no_overlap", "element_above", "contains_text", "text_fits_container",
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
        // This stub DUPLICATES `AssertRequest`'s hint rather than deriving
        // from it, so a variant added to the DSL has to be typed in twice.
        // Pinning the newest one is what makes the duplication survivable.
        assert!(
            types.iter().any(|t| t.as_str() == Some("element_above")),
            "element_above must be in allowed types"
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

#[cfg(test)]
mod json_5xx_code_stamping_tests {
    use super::{code_for_status, stamp_code_on_json_envelope};
    use axum::http::StatusCode;

    fn code_of(body: &str, status: StatusCode) -> Option<String> {
        let out = stamp_code_on_json_envelope(body.as_bytes(), status)?;
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        v["code"].as_str().map(str::to_string)
    }

    /// The reported defect: across 105 read-only GETs, all 7 of the 4xx
    /// carried a `code` and all 4 of the 5xx carried none — because
    /// `api_error()` builds `code: None` and the middleware skipped anything
    /// already `application/json`.
    #[test]
    fn a_code_less_json_5xx_envelope_gets_a_typed_code() {
        assert_eq!(
            code_of(
                r#"{"success":false,"error":"Python executor not running"}"#,
                StatusCode::INTERNAL_SERVER_ERROR
            ),
            Some("INTERNAL_ERROR".to_string())
        );
        assert_eq!(
            code_of(
                r#"{"success":false,"error":"cloud registry poll failed"}"#,
                StatusCode::BAD_GATEWAY
            ),
            Some("BAD_GATEWAY".to_string())
        );
        assert_eq!(
            code_of(
                r#"{"success":false,"error":"executor down"}"#,
                StatusCode::SERVICE_UNAVAILABLE
            ),
            Some("SERVICE_UNAVAILABLE".to_string())
        );
    }

    /// A handler that picked a precise code outranks the status-derived
    /// guess, so its body must come back untouched.
    #[test]
    fn an_existing_code_is_never_overwritten() {
        assert!(stamp_code_on_json_envelope(
            r#"{"success":false,"error":"x","code":"PYTHON_EXECUTOR_NOT_RUNNING"}"#.as_bytes(),
            StatusCode::SERVICE_UNAVAILABLE
        )
        .is_none());
    }

    /// Only `ApiResponse` failures are ours to edit. A 5xx whose body is some
    /// other JSON shape (GraphQL errors, a proxied upstream payload) must
    /// pass through byte-for-byte.
    #[test]
    fn foreign_json_shapes_are_left_alone() {
        assert!(stamp_code_on_json_envelope(
            r#"{"errors":[{"message":"boom"}]}"#.as_bytes(),
            StatusCode::INTERNAL_SERVER_ERROR
        )
        .is_none());
        assert!(stamp_code_on_json_envelope(
            r#"{"success":true,"data":{}}"#.as_bytes(),
            StatusCode::INTERNAL_SERVER_ERROR
        )
        .is_none());
        assert!(
            stamp_code_on_json_envelope(b"not json at all", StatusCode::INTERNAL_SERVER_ERROR)
                .is_none()
        );
    }

    /// The rest of the envelope must survive the rewrite intact.
    #[test]
    fn stamping_preserves_the_original_fields() {
        let out = stamp_code_on_json_envelope(
            r#"{"success":false,"error":"boom","data":{"k":1}}"#.as_bytes(),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .expect("should stamp");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["error"], serde_json::json!("boom"));
        assert_eq!(v["data"]["k"], serde_json::json!(1));
        assert_eq!(v["success"], serde_json::json!(false));
    }

    /// `code_for_status` is shared with the text/plain rewrite, so pin that
    /// the historical mapping is unchanged — extending the JSON pass must not
    /// have moved any code already on the wire.
    #[test]
    fn code_for_status_matches_the_historical_mapping() {
        assert_eq!(code_for_status(StatusCode::BAD_REQUEST), "INVALID_JSON");
        assert_eq!(
            code_for_status(StatusCode::UNPROCESSABLE_ENTITY),
            "INVALID_REQUEST"
        );
        assert_eq!(
            code_for_status(StatusCode::INTERNAL_SERVER_ERROR),
            "INTERNAL_ERROR"
        );
        assert_eq!(code_for_status(StatusCode::BAD_GATEWAY), "BAD_GATEWAY");
        assert_eq!(
            code_for_status(StatusCode::GATEWAY_TIMEOUT),
            "GATEWAY_TIMEOUT"
        );
        // Unnamed 4xx keep the historical catch-all.
        assert_eq!(code_for_status(StatusCode::NOT_FOUND), "BAD_REQUEST");
        // Unnamed 5xx are internal errors.
        assert_eq!(
            code_for_status(StatusCode::INSUFFICIENT_STORAGE),
            "INTERNAL_ERROR"
        );
    }
}
