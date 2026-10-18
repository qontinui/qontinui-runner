//! Proxy for secondary runners to forward `/wrappers/*` requests to the
//! primary runner.
//!
//! Mirrors `crate::process_capture::primary_proxy` 1:1 — same `Lazy<Client>`
//! pattern, same `ApiResponse<T>` envelope, same per-route typed `async fn`s.
//!
//! Phase 1 added the read-side: list, detail, status, credentials.
//! Phase 2 adds the write-side: install / update / uninstall / start /
//! stop / dispatch / set_credential / delete_credential. It also moves
//! the `ProxyError → HTTP` mapping into [`into_http`] so both
//! `routes.rs` and `dispatch.rs` reuse a single helper.
//!
//! The proxy gate is `is_secondary()` (re-exported from
//! `process_capture::primary_proxy` as the single source of truth) — a
//! strong check that requires both `QONTINUI_INSTANCE_NAME` and
//! `QONTINUI_PRIMARY_PORT` to be set.

use std::time::Duration;

use axum::response::Json as AxumJson;
use once_cell::sync::Lazy;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::clients::{AiClientId, ClientInfo};
use super::dispatch::DispatchResponse;
use super::install::{InstallRequest, InstallResponse};
use super::manager::WrapperStatus;
use super::registry::Wrapper;
use crate::mcp::types::{api_error, ApiResponse as McpApiResponse};

// Re-export the existing primary-detection helpers so callers in
// `wrappers::*` only depend on this module.
pub use crate::process_capture::primary_proxy::{is_secondary, primary_port};

/// Response wrapper matching the primary runner's `ApiResponse<T>` format.
#[derive(Deserialize)]
struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

/// Shared HTTP client for all proxy requests (reuses connections). Every
/// proxy client is loopback-only (`no_proxy`): a configured HTTP_PROXY must
/// never carry a call to the primary on 127.0.0.1.
static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .pool_max_idle_per_host(4)
        .build()
        .expect("Failed to create HTTP client")
});

fn client() -> &'static reqwest::Client {
    &CLIENT
}

/// `http://127.0.0.1:<primary_port>` — matches
/// `process_capture::primary_proxy::base_url` so both proxy modules share
/// the `9876` fallback.
fn base_url() -> String {
    let port = primary_port().unwrap_or(9876);
    format!("http://127.0.0.1:{}", port)
}

/// Errors raised by a proxy call. Carries the upstream HTTP status so the
/// secondary's HTTP handler can map a primary 404 → secondary 404 instead
/// of collapsing it into a generic 503.
///
/// `NotFound` is split out specifically because `get_wrapper` and
/// credential routes care about distinguishing "primary said no such
/// wrapper" from "primary unreachable / other error."
#[derive(Debug)]
pub enum ProxyError {
    /// Upstream primary returned 404 (or an `ApiResponse` with that
    /// status). Secondary should map this to a 404, not a 503.
    NotFound(String),
    /// The request never reached the primary: the connect failed (or the
    /// client could not be built). Nothing was started there, so this is the
    /// one proxy failure a caller may park on (HTTP 503 with
    /// [`PRIMARY_UNREACHABLE_CODE`]).
    Unreachable(String),
    /// Anything else: a send that failed or timed out AFTER connecting (the
    /// primary may have acted on it), a parse error, a non-success
    /// envelope, 5xx, etc. Carries the upstream HTTP status when the
    /// failure originated from a non-success response so the handler can
    /// preserve it; `None` means we never got a parsable response.
    Other {
        status: Option<StatusCode>,
        message: String,
        /// The upstream envelope's machine-readable `code` (e.g. the
        /// dispatch's `wrapper_busy`), carried through so a caller of the
        /// secondary can branch on it exactly as on the primary.
        code: Option<String>,
    },
}

/// The typed code of a proxy 503 whose request never reached the primary.
pub const PRIMARY_UNREACHABLE_CODE: &str = "primary_unreachable";

impl ProxyError {
    /// A failed send, split on whether the request can have reached the
    /// primary: a connect failure cannot (`Unreachable`); a timeout or any
    /// other failure after connecting can (`Other`, answered 502).
    fn send(e: reqwest::Error) -> Self {
        if e.is_connect() {
            Self::Unreachable(format!("Failed to reach primary runner: {}", e))
        } else {
            Self::other(format!(
                "primary runner did not answer after the request was sent: {}",
                e
            ))
        }
    }

    fn other(message: impl Into<String>) -> Self {
        Self::Other {
            status: None,
            message: message.into(),
            code: None,
        }
    }

    fn from_status(status: StatusCode, message: String, code: Option<String>) -> Self {
        if status == StatusCode::NOT_FOUND {
            ProxyError::NotFound(message)
        } else {
            ProxyError::Other {
                status: Some(status),
                message,
                code,
            }
        }
    }
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProxyError::NotFound(m) | ProxyError::Unreachable(m) => write!(f, "{}", m),
            ProxyError::Other { message, .. } => write!(f, "{}", message),
        }
    }
}

impl std::error::Error for ProxyError {}

/// Helper: send a GET, then parse the `ApiResponse<T>` envelope.
///
/// Status code preservation matters here — we check the HTTP status
/// before deserializing so a primary 404 maps to `ProxyError::NotFound`
/// even if the body parses as `ApiResponse<T> { success: false, ... }`.
async fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, ProxyError> {
    let resp = client().get(url).send().await.map_err(ProxyError::send)?;
    parse_response::<T>(resp).await
}

/// Proxy: `GET /wrappers`
pub async fn list_wrappers() -> Result<Vec<Wrapper>, ProxyError> {
    let url = format!("{}/wrappers", base_url());
    get_json::<Vec<Wrapper>>(&url).await
}

/// Proxy: `GET /wrappers/{id}`
///
/// Returns `ProxyError::NotFound` when the primary 404s so the handler
/// can map this to a secondary 404 (not a 503).
pub async fn get_wrapper(id: &str) -> Result<Wrapper, ProxyError> {
    let url = format!("{}/wrappers/{}", base_url(), id);
    get_json::<Wrapper>(&url).await
}

/// Proxy: `GET /wrappers/{id}/status`
pub async fn wrapper_status(id: &str) -> Result<WrapperStatus, ProxyError> {
    let url = format!("{}/wrappers/{}/status", base_url(), id);
    get_json::<WrapperStatus>(&url).await
}

/// Proxy: `GET /wrappers/{id}/credentials`
///
/// The `CredentialDescriptor` type is private to `wrappers::routes`, so
/// the secondary just passes the JSON through as `serde_json::Value` —
/// the UI deserializes whatever shape the primary sent.
pub async fn list_credentials(id: &str) -> Result<Vec<serde_json::Value>, ProxyError> {
    let url = format!("{}/wrappers/{}/credentials", base_url(), id);
    get_json::<Vec<serde_json::Value>>(&url).await
}

// ---------------------------------------------------------------------------
// Phase 2 — write-side proxies
// ---------------------------------------------------------------------------

/// Common envelope for any HTTP response that we receive from the primary
/// runner. Same shape as [`get_json`] but for verbs with a request body.
///
/// Status preservation matters here for the same reason as the GET path —
/// a primary 404 (e.g. `set_credential` on a missing wrapper) maps to a
/// secondary 404, not a 503.
async fn parse_response<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<T, ProxyError> {
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        return Err(failure_from_body(status, &body_text));
    }

    let api: ApiResponse<T> = resp
        .json()
        .await
        .map_err(|e| ProxyError::other(format!("Failed to parse primary response: {}", e)))?;

    if api.success {
        api.data
            .ok_or_else(|| ProxyError::other("primary response missing `data` field".to_string()))
    } else {
        Err(ProxyError::other(
            api.error.unwrap_or_else(|| "Unknown error".to_string()),
        ))
    }
}

/// The error for a primary's non-2xx answer. Only the envelope's typed
/// fields pass through — its `error` (the primary's own message) and `code`;
/// a body that is not the envelope is never forwarded, and the message is
/// then the STATUS's reason text, which comes from the status code alone.
fn failure_from_body(status: StatusCode, body_text: &str) -> ProxyError {
    let envelope = serde_json::from_str::<ApiResponse<serde_json::Value>>(body_text).ok();
    let code = envelope.as_ref().and_then(|env| env.code.clone());
    let message = envelope.and_then(|env| env.error).unwrap_or_else(|| {
        format!(
            "primary runner returned {} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("error")
        )
    });
    ProxyError::from_status(status, message, code)
}

/// Helper: send a POST with an optional JSON body and parse the
/// `ApiResponse<T>` envelope.
async fn post_json<R, B>(url: &str, body: Option<&B>) -> Result<R, ProxyError>
where
    R: for<'de> Deserialize<'de>,
    B: Serialize + ?Sized,
{
    let mut req = client().post(url);
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await.map_err(ProxyError::send)?;
    parse_response::<R>(resp).await
}

/// Helper: send a PUT with a JSON body and parse the `ApiResponse<T>` envelope.
async fn put_json<R, B>(url: &str, body: &B) -> Result<R, ProxyError>
where
    R: for<'de> Deserialize<'de>,
    B: Serialize + ?Sized,
{
    let resp = client()
        .put(url)
        .json(body)
        .send()
        .await
        .map_err(ProxyError::send)?;
    parse_response::<R>(resp).await
}

/// Helper: send a DELETE and parse the `ApiResponse<T>` envelope.
async fn delete_json<R: for<'de> Deserialize<'de>>(url: &str) -> Result<R, ProxyError> {
    let resp = client()
        .delete(url)
        .send()
        .await
        .map_err(ProxyError::send)?;
    parse_response::<R>(resp).await
}

/// Proxy: `POST /wrappers/install`.
///
/// Note: `pnpm install` can take a while on a slow network — the static
/// 10s `CLIENT` timeout is too tight. Use a fresh long-timeout client.
pub async fn install_wrapper(req: &InstallRequest) -> Result<InstallResponse, ProxyError> {
    let url = format!("{}/wrappers/install", base_url());
    let long_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| ProxyError::Unreachable(format!("Failed to create HTTP client: {}", e)))?;
    let resp = long_client
        .post(&url)
        .json(req)
        .send()
        .await
        .map_err(ProxyError::send)?;
    parse_response::<InstallResponse>(resp).await
}

/// Body shape for `POST /wrappers/{id}/update`. Mirrors the private
/// `UpdateRequest` in `routes.rs` so the proxy can serialize it.
#[derive(Serialize)]
struct UpdateBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'a str>,
}

/// Proxy: `POST /wrappers/{id}/update`.
pub async fn update_wrapper(
    id: &str,
    version: Option<&str>,
) -> Result<InstallResponse, ProxyError> {
    let url = format!("{}/wrappers/{}/update", base_url(), id);
    let body = UpdateBody { version };
    let long_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| ProxyError::Unreachable(format!("Failed to create HTTP client: {}", e)))?;
    let resp = long_client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(ProxyError::send)?;
    parse_response::<InstallResponse>(resp).await
}

/// Proxy: `DELETE /wrappers/{id}`. Returns the literal success string
/// (`"uninstalled"`) so the secondary handler can pass it through as
/// `&'static str`.
pub async fn uninstall_wrapper(id: &str) -> Result<String, ProxyError> {
    let url = format!("{}/wrappers/{}", base_url(), id);
    delete_json::<String>(&url).await
}

/// Deserialize-only mirror of `routes::StartedResponse` so the secondary
/// can pull the `port` field out without exposing a private type.
#[derive(Deserialize)]
struct StartedResponseDe {
    port: u16,
}

/// Proxy: `POST /wrappers/{id}/start`. Returns the assigned port.
pub async fn start_wrapper(id: &str) -> Result<u16, ProxyError> {
    let url = format!("{}/wrappers/{}/start", base_url(), id);
    let started: StartedResponseDe = post_json::<StartedResponseDe, ()>(&url, None).await?;
    Ok(started.port)
}

/// Proxy: `POST /wrappers/{id}/stop`. Returns `"stopped"`.
pub async fn stop_wrapper(id: &str) -> Result<String, ProxyError> {
    let url = format!("{}/wrappers/{}/stop", base_url(), id);
    post_json::<String, ()>(&url, None).await
}

/// Body shape for `PUT /wrappers/{id}/credentials/{name}`. Mirrors the
/// private `SetCredentialRequest` in `routes.rs`.
#[derive(Serialize)]
struct SetCredentialBody<'a> {
    value: &'a str,
}

/// Proxy: `PUT /wrappers/{id}/credentials/{name}`. Returns `"set"`.
pub async fn set_credential(id: &str, name: &str, value: &str) -> Result<String, ProxyError> {
    let url = format!("{}/wrappers/{}/credentials/{}", base_url(), id, name);
    let body = SetCredentialBody { value };
    put_json::<String, _>(&url, &body).await
}

/// Proxy: `DELETE /wrappers/{id}/credentials/{name}`. Returns `"deleted"`.
pub async fn delete_credential(id: &str, name: &str) -> Result<String, ProxyError> {
    let url = format!("{}/wrappers/{}/credentials/{}", base_url(), id, name);
    delete_json::<String>(&url).await
}

// ---------------------------------------------------------------------------
// AI client proxies
// ---------------------------------------------------------------------------

/// Render an `AiClientId` as its kebab-case URL segment.
fn client_id_segment(id: AiClientId) -> String {
    serde_json::to_value(id)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .expect("AiClientId serializes as a string via kebab-case rename")
}

/// Proxy: `GET /wrappers/clients`.
pub async fn list_clients() -> Result<Vec<ClientInfo>, ProxyError> {
    let url = format!("{}/wrappers/clients", base_url());
    get_json::<Vec<ClientInfo>>(&url).await
}

/// Proxy: `POST /wrappers/clients/{id}/connect`.
pub async fn connect_client(id: AiClientId) -> Result<(), ProxyError> {
    let url = format!(
        "{}/wrappers/clients/{}/connect",
        base_url(),
        client_id_segment(id)
    );
    let _: String = post_json::<String, ()>(&url, None).await?;
    Ok(())
}

/// Proxy: `POST /wrappers/clients/{id}/disconnect`.
pub async fn disconnect_client(id: AiClientId) -> Result<(), ProxyError> {
    let url = format!(
        "{}/wrappers/clients/{}/disconnect",
        base_url(),
        client_id_segment(id)
    );
    let _: String = post_json::<String, ()>(&url, None).await?;
    Ok(())
}

/// Proxy: `POST /wrappers/{id}/dispatch`.
///
/// The dispatch route is special — it carries an
/// `X-Wrapper-Dispatch-Timeout` header that callers use to override the
/// default 60s per-call timeout. The proxy must:
///
/// 1. Forward the header verbatim so the primary's `dispatch_handler`
///    sees the same per-call timeout the original caller asked for.
/// 2. Use a fresh `reqwest::Client` with a network timeout slightly
///    larger than `timeout` so the static `CLIENT`'s 10s default doesn't
///    cut off a slow long-running dispatch.
///
/// The `timeout` argument must be the same `Duration` produced by
/// `dispatch::resolve_timeout(headers)` — DON'T re-parse the header here.
/// The secondary handler passes its already-resolved value through.
///
/// Returns the typed [`DispatchResponse`] (the `data` field of the
/// primary's `ApiResponse`); the secondary's handler re-wraps in
/// `ApiResponse::success`.
pub async fn dispatch(
    id: &str,
    req: &serde_json::Value,
    timeout: Duration,
) -> Result<DispatchResponse, ProxyError> {
    let url = format!("{}/wrappers/{}/dispatch", base_url(), id);
    // The primary may spend up to PRE_DISPATCH_CEILING before its own
    // per-call timeout starts (the exclusive turn, the spawn lock, the spawn),
    // so the cushion covers that plus 30 s of network round-trip.
    let net_timeout = timeout
        .saturating_add(super::manager::PRE_DISPATCH_CEILING)
        .saturating_add(Duration::from_secs(30));
    let dispatch_client = reqwest::Client::builder()
        .no_proxy()
        .timeout(net_timeout)
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|e| ProxyError::Unreachable(format!("Failed to create HTTP client: {}", e)))?;
    let resp = dispatch_client
        .post(&url)
        .header(
            super::dispatch::TIMEOUT_HEADER,
            timeout.as_secs().to_string(),
        )
        .json(req)
        .send()
        .await
        .map_err(ProxyError::send)?;
    parse_response::<DispatchResponse>(resp).await
}

// ---------------------------------------------------------------------------
// Error → HTTP mapping
// ---------------------------------------------------------------------------

/// Convert a [`ProxyError`] into the standard error envelope. Lives here
/// (rather than in `routes.rs`) so both `routes.rs` and `dispatch.rs` can
/// reuse it without introducing a cross-module visibility hop.
///
/// - `NotFound` → 404 (preserves "missing wrapper" semantics).
/// - `Unreachable` (the request never reached the primary) → 503 with the
///   typed code [`PRIMARY_UNREACHABLE_CODE`] and the "primary runner is not
///   reachable" message that matches the `require_wrapper_state` shape.
/// - A failure after the request was sent, or an unparsable answer (no
///   upstream status) → 502: the primary may have acted on it.
/// - Upstream HTTP 4xx (client error from primary) → forwarded as-is so
///   the secondary's caller sees the same status the primary returned.
///   A primary-side `400 BAD_REQUEST` for a malformed `InstallRequest`
///   stays a 400 instead of becoming a confusing 502.
/// - Upstream HTTP 503 carrying a typed "nothing started" code → passed
///   through with its code.
/// - Any other upstream 5xx, an untyped 503 included → 502 BAD_GATEWAY (the
///   secondary failed to get a useful response from the primary).
pub fn into_http(e: ProxyError) -> (StatusCode, AxumJson<McpApiResponse<()>>) {
    match e {
        ProxyError::NotFound(msg) => (StatusCode::NOT_FOUND, AxumJson(api_error(msg))),
        ProxyError::Unreachable(message) => (
            StatusCode::SERVICE_UNAVAILABLE,
            AxumJson(McpApiResponse::error_with_code(
                format!(
                    "primary runner is not reachable; install wrappers from the primary's UI ({})",
                    message
                ),
                PRIMARY_UNREACHABLE_CODE,
            )),
        ),
        ProxyError::Other {
            status: None,
            message,
            ..
        } => (
            StatusCode::BAD_GATEWAY,
            AxumJson(api_error(format!(
                "primary runner gave no usable answer ({})",
                message
            ))),
        ),
        ProxyError::Other {
            status: Some(s),
            message,
            code,
        } => {
            // 4xx upstream is the caller's fault — pass it through so the
            // status describes the underlying problem. A 503 passes through
            // ONLY when it carries one of the primary's typed "nothing
            // started" codes (`dispatch::NOTHING_STARTED_CODES`), which a
            // caller parks on. An untyped 503 (an older primary, or anything
            // else answering on that port) is not known to mean "nothing
            // started", so like every other 5xx it becomes 502.
            let typed_nothing_started = code
                .as_deref()
                .is_some_and(|c| super::dispatch::NOTHING_STARTED_CODES.contains(&c));
            let mapped = if s.is_client_error()
                || (s == StatusCode::SERVICE_UNAVAILABLE && typed_nothing_started)
            {
                s
            } else {
                StatusCode::BAD_GATEWAY
            };
            let body = match code {
                Some(code) => McpApiResponse::error_with_code(message, code),
                None => api_error(message),
            };
            (mapped, AxumJson(body))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Through a secondary runner the primary's typed verdicts survive: a
    /// 503 with a "nothing started" code stays a 503 with it, a typed 429
    /// busy stays a 429 with its code. An UNTYPED 503 is not known to mean
    /// "nothing started" and becomes 502, like every other 5xx.
    #[test]
    fn the_primarys_typed_503_and_429_pass_through() {
        for code in super::super::dispatch::NOTHING_STARTED_CODES {
            let (s, body) = into_http(ProxyError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                "x".into(),
                Some(code.to_string()),
            ));
            assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{code}");
            assert_eq!(body.0.code.as_deref(), Some(*code));
        }
        for code in [None, Some("wrapper_busy".to_string())] {
            let (s, body) = into_http(ProxyError::from_status(
                StatusCode::SERVICE_UNAVAILABLE,
                "failed to spawn".into(),
                code.clone(),
            ));
            assert_eq!(s, StatusCode::BAD_GATEWAY, "{code:?}");
            assert_eq!(body.0.code, code);
        }
        let (s, body) = into_http(ProxyError::from_status(
            StatusCode::TOO_MANY_REQUESTS,
            "busy".into(),
            Some("wrapper_busy".into()),
        ));
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body.0.code.as_deref(), Some("wrapper_busy"));
        let (s, _) = into_http(ProxyError::from_status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "boom".into(),
            None,
        ));
        assert_eq!(s, StatusCode::BAD_GATEWAY);
    }

    /// A send that never connected is the typed 503 (nothing reached the
    /// primary: park); one that timed out AFTER connecting is 502 (the
    /// primary may have acted on it: count it).
    #[tokio::test]
    async fn a_connect_failure_is_a_typed_503_and_a_post_send_timeout_is_502() {
        // Closed port: bind, read the port, drop the listener.
        let port = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        let e = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!("http://127.0.0.1:{port}/x"))
            .send()
            .await
            .unwrap_err();
        let (s, body) = into_http(ProxyError::send(e));
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.0.code.as_deref(), Some(PRIMARY_UNREACHABLE_CODE));

        // A primary that accepts the connection and never answers.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(sock);
        });
        let e = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(300))
            .build()
            .unwrap()
            .post(format!("http://{addr}/x"))
            .body("{}")
            .send()
            .await
            .unwrap_err();
        assert!(e.is_timeout() && !e.is_connect(), "{e:?}");
        let (s, body) = into_http(ProxyError::send(e));
        assert_eq!(s, StatusCode::BAD_GATEWAY);
        assert_eq!(body.0.code, None);
        held.abort();
    }

    /// A non-envelope body from the primary (a proxy's HTML page, a crash
    /// dump) is never forwarded: the message is the status's reason text.
    /// Through `parse_response`, the ONE error path both GET and the write
    /// verbs take.
    #[tokio::test]
    async fn an_unparsed_primary_body_is_never_forwarded() {
        let secret = concat!("SID=", "s3cr3tCookieValue");
        let resp: reqwest::Response = axum::http::Response::builder()
            .status(502)
            .body(format!("<html>upstream said {secret}</html>"))
            .unwrap()
            .into();
        let err = parse_response::<serde_json::Value>(resp).await.unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains(secret), "{msg}");
        assert!(!msg.contains("html"), "{msg}");
        assert_eq!(msg, "primary runner returned 502 Bad Gateway");

        // The envelope's typed fields DO pass.
        let resp: reqwest::Response = axum::http::Response::builder()
            .status(503)
            .body(r#"{"success":false,"error":"failed to spawn wrapper 'w'","code":"wrapper_spawn_failed"}"#)
            .unwrap()
            .into();
        match parse_response::<serde_json::Value>(resp).await.unwrap_err() {
            ProxyError::Other {
                status,
                message,
                code,
            } => {
                assert_eq!(status, Some(StatusCode::SERVICE_UNAVAILABLE));
                assert_eq!(message, "failed to spawn wrapper 'w'");
                assert_eq!(code.as_deref(), Some("wrapper_spawn_failed"));
            }
            other => panic!("{other:?}"),
        }
    }
}
