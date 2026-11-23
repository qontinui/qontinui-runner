//! The client a stored SDK connection sends through.
//!
//! For a phone that requires pairing ([`crate::mcp::transport::pairing`]) every
//! request is signed here, over the bytes it actually carries, just before it goes
//! out. Any other connection sends exactly as a plain `reqwest::Client` would.
//!
//! Two things are added for a signed connection:
//! - A `PAIRING_TIMESTAMP_REFUSED` answer carries the phone's clock. The request is
//!   re-signed against it and sent once more.
//! - A phone's other `PAIRING_*` refusals name the phone's own routes. Their
//!   `error` is replaced with what to do on this runner, and the phone's text is
//!   kept as `deviceError`.

use std::sync::Arc;
use std::time::Duration;

use axum::http;
use reqwest::header::{HeaderName, HeaderValue, CONTENT_LENGTH};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use tracing::{debug, warn};

use crate::mcp::transport::pairing::{
    explain_refusal, DeviceSigner, PAIRING_TIMESTAMP_REFUSED, UI_BRIDGE_AUTH_HEADER,
};

/// A connection's HTTP client: `reqwest`'s, plus the phone's signer when the
/// phone requires pairing. Cheap to clone.
#[derive(Clone)]
pub struct SdkHttpClient {
    http: reqwest::Client,
    signer: Option<Arc<DeviceSigner>>,
}

impl SdkHttpClient {
    /// A client that sends every request unsigned.
    pub fn unsigned(http: reqwest::Client) -> Self {
        Self { http, signer: None }
    }

    /// A client that signs every request for the signer's phone.
    pub fn signed(http: reqwest::Client, signer: Arc<DeviceSigner>) -> Self {
        Self {
            http,
            signer: Some(signer),
        }
    }

    /// The phone this client signs for, if any.
    pub fn signer(&self) -> Option<&Arc<DeviceSigner>> {
        self.signer.as_ref()
    }

    /// This client without its signer, for the phone's open routes
    /// (`/ui-bridge/health`). A signature the phone refuses there counts against
    /// this runner as on any other route, so a stale token would put the runner on
    /// the phone's backoff before the request it was probing for.
    pub fn without_signer(&self) -> Self {
        Self::unsigned(self.http.clone())
    }

    /// This client with its `reqwest` client replaced, keeping the signer.
    pub fn with_http(&self, http: reqwest::Client) -> Self {
        Self {
            http,
            signer: self.signer.clone(),
        }
    }

    pub fn request<U: reqwest::IntoUrl>(&self, method: Method, url: U) -> SdkRequest {
        SdkRequest {
            builder: self.http.request(method, url),
            client: self.clone(),
        }
    }

    pub fn get<U: reqwest::IntoUrl>(&self, url: U) -> SdkRequest {
        self.request(Method::GET, url)
    }

    pub fn post<U: reqwest::IntoUrl>(&self, url: U) -> SdkRequest {
        self.request(Method::POST, url)
    }

    pub fn put<U: reqwest::IntoUrl>(&self, url: U) -> SdkRequest {
        self.request(Method::PUT, url)
    }

    pub fn delete<U: reqwest::IntoUrl>(&self, url: U) -> SdkRequest {
        self.request(Method::DELETE, url)
    }
}

/// One request on an [`SdkHttpClient`], built as a `reqwest::RequestBuilder` is.
pub struct SdkRequest {
    client: SdkHttpClient,
    builder: reqwest::RequestBuilder,
}

impl SdkRequest {
    pub fn header<K, V>(self, key: K, value: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        Self {
            builder: self.builder.header(key, value),
            ..self
        }
    }

    pub fn json<T: Serialize + ?Sized>(self, json: &T) -> Self {
        Self {
            builder: self.builder.json(json),
            ..self
        }
    }

    pub fn body<T: Into<reqwest::Body>>(self, body: T) -> Self {
        Self {
            builder: self.builder.body(body),
            ..self
        }
    }

    pub fn query<T: Serialize + ?Sized>(self, query: &T) -> Self {
        Self {
            builder: self.builder.query(query),
            ..self
        }
    }

    pub fn timeout(self, timeout: Duration) -> Self {
        Self {
            builder: self.builder.timeout(timeout),
            ..self
        }
    }

    /// Send it, signed when the client has a signer.
    pub async fn send(self) -> reqwest::Result<reqwest::Response> {
        let Some(signer) = self.client.signer.clone() else {
            return self.builder.send().await;
        };
        let mut request = self.builder.build()?;
        let retry = request.try_clone();
        if let Some(refused) = sign_request(&signer, &mut request) {
            return Ok(refused);
        }
        let response = self.client.http.execute(request).await?;
        let (response, refusal) = read_refusal(response).await?;
        let Some(refusal) = refusal else {
            return Ok(response);
        };
        if refusal.code == PAIRING_TIMESTAMP_REFUSED {
            if let (Some(server_time), Some(mut retry)) = (refusal.server_time_ms, retry) {
                debug!(
                    device_id = signer.device_id(),
                    "the device refused the signature's timestamp; re-signing on its clock"
                );
                signer.resync(server_time);
                if let Some(refused) = sign_request(&signer, &mut retry) {
                    return Ok(refused);
                }
                let response = self.client.http.execute(retry).await?;
                let (response, refusal) = read_refusal(response).await?;
                return Ok(match refusal {
                    Some(refusal) => explained(refusal, signer.device_id()),
                    None => response,
                });
            }
        }
        Ok(explained(refusal, signer.device_id()))
    }
}

/// Put the signature on `request`. A body that is not in memory cannot be signed:
/// the answer is then a refusal made here rather than a request the phone would
/// refuse.
fn sign_request(
    signer: &DeviceSigner,
    request: &mut reqwest::Request,
) -> Option<reqwest::Response> {
    let body: &[u8] =
        match request.body() {
            None => &[],
            Some(body) => match body.as_bytes() {
                Some(bytes) => bytes,
                None => return Some(local_refusal(
                    "PAIRING_UNSIGNABLE_BODY",
                    "A streamed request body cannot be signed for a device that requires pairing",
                )),
            },
        };
    let url = request.url();
    let path_and_query = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let header = signer.sign(request.method().as_str(), &path_and_query, body)?;
    match HeaderValue::from_str(&header) {
        Ok(mut value) => {
            value.set_sensitive(true);
            request
                .headers_mut()
                .insert(HeaderName::from_static("x-ui-bridge-auth"), value);
        }
        Err(e) => warn!("{UI_BRIDGE_AUTH_HEADER} could not be built ({e}); sending unsigned"),
    }
    None
}

/// A `PAIRING_*` answer from the phone.
struct Refusal {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: serde_json::Value,
    code: String,
    server_time_ms: Option<i64>,
}

/// The statuses a phone's pairing gate answers with.
fn is_refusal_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 413 | 414 | 429 | 503)
}

/// Split a phone's `PAIRING_*` refusal off `response`. Any other answer comes back
/// as it was, its body read into memory only when its status is one a refusal
/// uses.
async fn read_refusal(
    response: reqwest::Response,
) -> reqwest::Result<(reqwest::Response, Option<Refusal>)> {
    let status = response.status();
    if !is_refusal_status(status) {
        return Ok((response, None));
    }
    let version = response.version();
    let headers = response.headers().clone();
    let bytes = response.bytes().await?;
    let parsed = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .filter(|body| {
            body.get("code")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|code| code.starts_with("PAIRING_"))
        });
    match parsed {
        Some(body) => {
            let code = body["code"].as_str().unwrap_or_default().to_string();
            let server_time_ms = body
                .get("data")
                .and_then(|data| data.get("serverTime"))
                .and_then(serde_json::Value::as_i64);
            Ok((
                rebuilt(status, version, &headers, bytes.to_vec()),
                Some(Refusal {
                    status,
                    headers,
                    body,
                    code,
                    server_time_ms,
                }),
            ))
        }
        None => Ok((rebuilt(status, version, &headers, bytes.to_vec()), None)),
    }
}

/// The phone's refusal, with `error` saying what to do on this runner.
fn explained(refusal: Refusal, device_id: &str) -> reqwest::Response {
    let Refusal {
        status,
        headers,
        mut body,
        code,
        ..
    } = refusal;
    debug!(device_id, code = %code, status = status.as_u16(), "the device refused a signed request");
    if let (Some(explanation), Some(object)) = (
        explain_refusal(&code, Some(device_id)),
        body.as_object_mut(),
    ) {
        let device_error = object.remove("error");
        object.insert("error".to_string(), serde_json::Value::String(explanation));
        if let Some(device_error) = device_error {
            object.insert("deviceError".to_string(), device_error);
        }
    }
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    rebuilt(status, http::Version::HTTP_11, &headers, bytes)
}

fn local_refusal(code: &str, message: &str) -> reqwest::Response {
    let body = serde_json::json!({ "success": false, "error": message, "code": code });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    rebuilt(
        StatusCode::BAD_REQUEST,
        http::Version::HTTP_11,
        &headers,
        serde_json::to_vec(&body).unwrap_or_default(),
    )
}

/// A response with `body` in place of whatever was read. Its `url()` no longer
/// names the target, which nothing on a refusal path reads.
fn rebuilt(
    status: StatusCode,
    version: http::Version,
    headers: &reqwest::header::HeaderMap,
    body: Vec<u8>,
) -> reqwest::Response {
    let mut builder = http::Response::builder().status(status).version(version);
    for (name, value) in headers {
        if name != CONTENT_LENGTH {
            builder = builder.header(name, value);
        }
    }
    let response = builder
        .body(body)
        .unwrap_or_else(|_| http::Response::new(Vec::new()));
    reqwest::Response::from(response)
}

/// A client for one-off requests to the phone whose UI Bridge server is at
/// `base_url`, decided as a connect decides (see `sdk_client::device_client`): a
/// phone that requires pairing gets a signing client when it issued this runner a
/// token, and a refusal saying how to pair when it did not. Anything without a
/// pairing gate, including a server that does not answer the probe, is sent
/// unsigned through `http`.
pub async fn probe_device_client(
    base_url: &str,
    http: reqwest::Client,
) -> Result<SdkHttpClient, String> {
    use crate::mcp::transport::pairing::{device_tokens, pairing_required_message, DeviceGate};

    let key = base_url.trim_end_matches('/').to_string();
    let decision = match cached_probe(&key) {
        Some(decision) => decision,
        None => {
            let health_url = format!("{key}/ui-bridge/health");
            let health = match http
                .get(&health_url)
                .timeout(Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) => resp.json::<serde_json::Value>().await.ok(),
                Err(_) => None,
            };
            let decision = match health.as_ref().and_then(DeviceGate::from_health) {
                Some(gate) if gate.required => {
                    let Some(device_id) = gate.device_id else {
                        return Err(format!(
                            "{base_url} requires pairing but its health answer names no deviceId"
                        ));
                    };
                    ProbeDecision::Sign {
                        device_id,
                        clock_offset_ms: gate
                            .server_time_ms
                            .map(|t| t - chrono::Utc::now().timestamp_millis()),
                    }
                }
                Some(_) | None => ProbeDecision::Unsigned,
            };
            // Only an answer is remembered: a probe that failed or did not parse
            // says nothing about the device, and remembering it as unsigned would
            // send a paired phone unsigned requests for the whole window. A phone
            // this runner cannot sign for yet is not remembered either, so a
            // pairing is used from the next frame on.
            let signable = match &decision {
                ProbeDecision::Sign { device_id, .. } => device_tokens().has_token(device_id),
                ProbeDecision::Unsigned => true,
            };
            if health.is_some() && signable {
                remember_probe(key, decision.clone());
            }
            decision
        }
    };
    match decision {
        ProbeDecision::Unsigned => Ok(SdkHttpClient::unsigned(http)),
        ProbeDecision::Sign {
            device_id,
            clock_offset_ms,
        } => {
            let store = device_tokens();
            if !store.has_token(&device_id) {
                return Err(pairing_required_message(Some(&device_id)));
            }
            let server_time_ms =
                clock_offset_ms.map(|offset| chrono::Utc::now().timestamp_millis() + offset);
            let signer = Arc::new(DeviceSigner::new(device_id, store, server_time_ms));
            Ok(SdkHttpClient::signed(http, signer))
        }
    }
}

/// How long [`probe_device_client`] reuses what a device's health said, so a run
/// of frames costs one health round trip rather than one each.
const PROBE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
enum ProbeDecision {
    Unsigned,
    Sign {
        device_id: String,
        /// The phone's clock minus this machine's, when its health said.
        clock_offset_ms: Option<i64>,
    },
}

fn probe_cache() -> std::sync::MutexGuard<
    'static,
    std::collections::HashMap<String, (std::time::Instant, ProbeDecision)>,
> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, (std::time::Instant, ProbeDecision)>>,
    > = std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cached_probe(key: &str) -> Option<ProbeDecision> {
    let mut cache = probe_cache();
    cache.retain(|_, (at, _)| at.elapsed() < PROBE_TTL);
    cache.get(key).map(|(_, decision)| decision.clone())
}

fn remember_probe(key: String, decision: ProbeDecision) {
    probe_cache().insert(key, (std::time::Instant::now(), decision));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::transport::pairing::DeviceTokenStore;

    fn signed_client() -> SdkHttpClient {
        let store = Arc::new(DeviceTokenStore::in_memory());
        store.store("phone-x", "token-x", true).unwrap();
        let signer = DeviceSigner::new("phone-x".to_string(), store, None);
        SdkHttpClient::signed(reqwest::Client::new(), Arc::new(signer))
    }

    /// A body that is not in memory cannot be signed, so the refusal is made here
    /// and nothing is sent: the URL names a port nothing listens on, and the answer
    /// is still the local refusal rather than a connect error.
    #[tokio::test]
    async fn a_streamed_body_is_refused_here_rather_than_sent() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = held.local_addr().expect("local addr").port();
        drop(held);
        let stream = futures::stream::once(async {
            Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"{}"))
        });
        let resp = signed_client()
            .post(format!("http://127.0.0.1:{port}/ui-bridge/control/x"))
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .expect("a local refusal, not a send");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["code"], "PAIRING_UNSIGNABLE_BODY");
    }

    /// The open-route view keeps the `reqwest` client and drops only the signer.
    #[test]
    fn without_signer_drops_only_the_signer() {
        let client = signed_client();
        assert_eq!(client.signer().map(|s| s.device_id()), Some("phone-x"));
        assert!(client.without_signer().signer().is_none());
    }
}
