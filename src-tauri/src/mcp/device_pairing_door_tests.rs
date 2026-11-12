//! UI Bridge device pairing at the runner's doors: the connect handshake, the
//! stored connection every later request goes through, the physical-device
//! connect, and the pair call.
//!
//! The phone here is a stand-in that gates its routes the way
//! `@qontinui/ui-bridge-native`'s `src/server/pairing.ts` does: an open health
//! answer naming `deviceId`, `serverTime` and `pairing`, a `POST /ui-bridge/pair`,
//! and every other route refused unless `X-UI-Bridge-Auth` verifies over the
//! request with the token, inside +-300 s of the phone's clock, with a nonce it
//! has not seen. Verification uses [`signature_header`], which the unit tests in
//! `transport::pairing` pin to the phone's own published vector.
//!
//! The stored device JWT [`door`] leaves behind is what makes "never the bearer"
//! observable: it is there to be sent, and must not be.

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use tokio::net::TcpListener;

use super::sdk_connect_door_tests::{door, start_unreadable_adb, FakeAdb};
use crate::mcp::physical_device::{ActiveTransport, PhysicalDeviceInfo, PhysicalDeviceRegistry};
use crate::mcp::physical_device_api::{
    connect_device_at, connect_device_over_transports, pair_device_with,
};
use crate::mcp::sdk_client::{connect_sdk_app, recheck_stored_bearers, SdkConnectionManager};
use crate::mcp::transport::lan::LanTransport;
use crate::mcp::transport::pairing::{
    device_tokens, pair_over_local_forward, pair_with_device, signature_header, DeviceTokenStore,
    PairError, PairOptions,
};
use crate::mcp::transport::{
    mark_cable_forwarder, mark_off_machine_forwarder, DeviceOs, ForgetCableForwarder,
    ForgetForwarder, TransportKind,
};

/// One request the phone saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    auth: Option<String>,
    authorization: Option<String>,
    /// Every header, `name: value` per line, to search for the token.
    headers: String,
    body: Vec<u8>,
    status: u16,
}

/// The stand-in phone.
struct Phone {
    device_id: String,
    token: String,
    /// Admits every caller as on the phone itself, as over `adb forward`.
    local: bool,
    /// Refuses every signature, as a phone whose pairing the app revoked.
    revoked: bool,
    /// The code armed in the app's Settings, if any.
    code: Option<String>,
    /// The `deviceId` its pair answer names, when not its own: what anything on
    /// the network could answer.
    pair_device_id: Option<String>,
    /// Added to this machine's clock to make the phone's.
    skew_ms: AtomicI64,
    nonces: Mutex<HashSet<String>>,
    seen: Mutex<Vec<Seen>>,
}

impl Phone {
    fn new(local: bool) -> Self {
        let unique = uuid::Uuid::new_v4().simple().to_string();
        Self {
            device_id: format!("phone-{unique}"),
            token: format!("{unique}{unique}"),
            local,
            revoked: false,
            code: None,
            pair_device_id: None,
            skew_ms: AtomicI64::new(0),
            nonces: Mutex::default(),
            seen: Mutex::default(),
        }
    }

    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp_millis() + self.skew_ms.load(Ordering::SeqCst)
    }

    fn refusal(
        status: StatusCode,
        code: &str,
        extra: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let mut body = serde_json::json!({
            "success": false,
            "error": format!("the phone's own text for {code}: POST /ui-bridge/pair"),
            "code": code,
        });
        if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            body.extend(extra.clone());
        }
        (status, body)
    }

    fn pair(&self, body: &[u8]) -> (StatusCode, serde_json::Value) {
        if !self.local {
            let sent = serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .and_then(|body| body.get("code")?.as_str().map(str::to_string));
            match (&self.code, sent) {
                (None, _) => {
                    return Self::refusal(
                        StatusCode::UNAUTHORIZED,
                        "PAIRING_NOT_ARMED",
                        serde_json::json!({}),
                    )
                }
                (Some(_), None) => {
                    return Self::refusal(
                        StatusCode::UNAUTHORIZED,
                        "PAIRING_CODE_REQUIRED",
                        serde_json::json!({}),
                    )
                }
                (Some(armed), Some(sent)) if *armed != sent => {
                    return Self::refusal(
                        StatusCode::UNAUTHORIZED,
                        "PAIRING_CODE_INVALID",
                        serde_json::json!({}),
                    )
                }
                _ => {}
            }
        }
        (
            StatusCode::OK,
            serde_json::json!({
                "success": true,
                "data": {
                    "token": self.token,
                    "deviceId": self.pair_device_id.as_deref().unwrap_or(&self.device_id),
                    "serverTime": self.now(),
                },
            }),
        )
    }

    fn verify(
        &self,
        method: &Method,
        target: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(), (StatusCode, serde_json::Value)> {
        if self.local {
            return Ok(());
        }
        let required = || {
            Self::refusal(
                StatusCode::UNAUTHORIZED,
                "PAIRING_REQUIRED",
                serde_json::json!({}),
            )
        };
        let Some(header) = headers
            .get("x-ui-bridge-auth")
            .and_then(|v| v.to_str().ok())
        else {
            return Err(required());
        };
        let parts: Vec<&str> = header.split('.').collect();
        let [ts, nonce, _sig] = parts.as_slice() else {
            return Err(required());
        };
        let Ok(ts) = ts.parse::<i64>() else {
            return Err(required());
        };
        let now = self.now();
        if (now - ts).abs() > 300_000 {
            return Err(Self::refusal(
                StatusCode::UNAUTHORIZED,
                "PAIRING_TIMESTAMP_REFUSED",
                serde_json::json!({ "data": { "serverTime": now } }),
            ));
        }
        if self.revoked {
            return Err(required());
        }
        let expected = signature_header(&self.token, method.as_str(), target, ts, nonce, body);
        if expected != header {
            return Err(required());
        }
        if !self.nonces.lock().unwrap().insert(nonce.to_string()) {
            return Err(required());
        }
        Ok(())
    }

    fn answer(
        &self,
        method: Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: &[u8],
    ) -> (StatusCode, serde_json::Value) {
        let target = uri
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| uri.path().to_string());
        if !uri.path().starts_with("/ui-bridge/") {
            return (
                StatusCode::NOT_FOUND,
                serde_json::json!({ "success": false }),
            );
        }
        if uri.path() == "/ui-bridge/health" {
            return (
                StatusCode::OK,
                serde_json::json!({
                    "success": true,
                    "data": {
                        "status": "ok",
                        "deviceId": self.device_id,
                        "serverTime": self.now(),
                        "uiBridge": { "appId": "phone", "appName": "Phone", "appType": "mobile" },
                        "pairing": { "required": !self.local, "authenticated": self.local },
                    },
                }),
            );
        }
        if uri.path() == "/ui-bridge/pair" && method == Method::POST {
            return self.pair(body);
        }
        match self.verify(&method, &target, headers, body) {
            Ok(()) => (
                StatusCode::OK,
                serde_json::json!({
                    "success": true,
                    "data": { "elements": [{ "id": "greeting", "label": "Hello", "type": "text" }] },
                }),
            ),
            Err(refusal) => refusal,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// Requests to routes the gate covers: everything but health and pair.
    fn gated_requests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| !s.target.starts_with("/ui-bridge/health") && s.target != "/ui-bridge/pair")
            .filter(|s| s.target.starts_with("/ui-bridge/"))
            .collect()
    }
}

/// Serve `phone` on 127.0.0.1 and return its port.
async fn spawn_phone(phone: Arc<Phone>) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().fallback(
        move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
            let phone = phone.clone();
            async move {
                let (status, answer) = phone.answer(method.clone(), &uri, &headers, &body);
                let header = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned)
                };
                let all_headers = headers
                    .iter()
                    .map(|(name, value)| {
                        format!("{name}: {}", String::from_utf8_lossy(value.as_bytes()))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                phone.seen.lock().unwrap().push(Seen {
                    method: method.to_string(),
                    target: uri
                        .path_and_query()
                        .map(|pq| pq.as_str().to_string())
                        .unwrap_or_default(),
                    auth: header("x-ui-bridge-auth"),
                    authorization: header("authorization"),
                    headers: all_headers,
                    body: body.to_vec(),
                    status: status.as_u16(),
                });
                (status, axum::Json(answer))
            }
        },
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    port
}

/// The phone behind a LAN proxy, as `register-lan` dials one: its URL is this
/// machine's loopback, while the bytes reach the phone, which sees the runner's
/// address and so requires a signature.
async fn phone_behind_lan_proxy(phone: Arc<Phone>) -> (String, ForgetForwarder) {
    let phone_port = spawn_phone(phone).await;
    let (proxy_port, _proxy) = LanTransport::new()
        .create_proxy(std::net::SocketAddr::from(([127, 0, 0, 1], phone_port)))
        .await
        .expect("the LAN proxy starts");
    (
        format!("http://127.0.0.1:{proxy_port}"),
        ForgetForwarder(proxy_port),
    )
}

/// The phone behind a port marked as a cable this runner forwarded, as an `adb
/// forward` is: the phone admits the caller as on the phone itself.
async fn phone_over_a_cable(phone: Arc<Phone>) -> (String, ForgetCableForwarder) {
    let port = spawn_phone(phone).await;
    mark_cable_forwarder(port);
    (
        format!("http://127.0.0.1:{port}"),
        ForgetCableForwarder(port),
    )
}

/// The cloud relay's tunnel to a phone: marked as forwarding off this machine,
/// as `transport::cloud` marks its tunnels, and not a cable.
async fn phone_behind_relay(phone: Arc<Phone>) -> (String, ForgetForwarder) {
    let port = spawn_phone(phone).await;
    mark_off_machine_forwarder(port);
    (format!("http://127.0.0.1:{port}"), ForgetForwarder(port))
}

/// An operator's pairing with `code`, which may not replace a held token.
fn with_code(code: Option<&str>) -> PairOptions<'_> {
    PairOptions {
        code,
        explicit: true,
        ..PairOptions::default()
    }
}

fn assert_no_secret_crossed(phone: &Phone) {
    for seen in phone.seen() {
        assert_eq!(
            seen.authorization, None,
            "the operator's bearer reached the phone: {seen:?}"
        );
        assert!(
            !seen.headers.contains(&phone.token),
            "the token crossed the wire in a header: {seen:?}"
        );
        assert!(
            !String::from_utf8_lossy(&seen.body).contains(&phone.token),
            "the token crossed the wire in a body: {seen:?}"
        );
    }
}

/// A paired phone on Wi-Fi is connected `device-signed`, and every request after
/// the handshake carries a signature it accepts, GETs and bodies alike. Neither
/// the token nor the operator's bearer ever crosses the wire.
#[tokio::test]
async fn a_paired_phone_gets_a_signature_on_every_request_and_never_the_token_or_a_bearer() {
    let _door = door();
    let phone = Arc::new(Phone::new(false));
    device_tokens()
        .store(&phone.device_id, &phone.token, true)
        .unwrap();
    let (url, _forget) = phone_behind_lan_proxy(phone.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let connected = connect_device_at(&manager, "lan-paired".into(), url.clone())
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {:?}", e.error));
    assert_eq!(
        serde_json::to_value(&connected).unwrap()["authPath"],
        "device-signed"
    );

    let client = {
        let manager = manager.lock().await;
        let conn = manager
            .active_connection()
            .expect("the phone is the active target");
        assert_eq!(conn.base_path, "/ui-bridge");
        conn.client.clone()
    };
    let snapshot = client
        .get(format!(
            "{url}/ui-bridge/control/snapshot?visibleOnly=true&maxElements=200"
        ))
        .send()
        .await
        .expect("a snapshot");
    assert_eq!(snapshot.status(), 200);
    let action = client
        .post(format!(
            "{url}/ui-bridge/control/element/a%20b/action?x=1&y=%C3%A9"
        ))
        .json(&serde_json::json!({ "action": "click", "elementId": "é" }))
        .send()
        .await
        .expect("an action");
    assert_eq!(action.status(), 200);

    let gated = phone.gated_requests();
    assert_eq!(gated.len(), 2, "{gated:?}");
    assert!(
        gated.iter().all(|s| s.status == 200 && s.auth.is_some()),
        "{gated:?}"
    );
    let nonces: HashSet<_> = gated
        .iter()
        .map(|s| {
            s.auth
                .as_deref()
                .unwrap()
                .split('.')
                .nth(1)
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(nonces.len(), 2, "every request gets its own nonce");
    assert_no_secret_crossed(&phone);
}

/// A capture of a signed request is refused when replayed, and on any other
/// route: the signature binds the method, target and body, and its nonce is
/// spent by the request it came with.
#[tokio::test]
async fn a_captured_signature_is_refused_when_replayed_or_moved() {
    let _door = door();
    let phone = Arc::new(Phone::new(false));
    device_tokens()
        .store(&phone.device_id, &phone.token, true)
        .unwrap();
    let (url, _forget) = phone_behind_lan_proxy(phone.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    connect_device_at(&manager, "lan-replay".into(), url.clone())
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {:?}", e.error));
    let client = manager
        .lock()
        .await
        .active_connection()
        .unwrap()
        .client
        .clone();
    let ok = client
        .get(format!("{url}/ui-bridge/control/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    let captured = phone.gated_requests()[0].auth.clone().expect("signed");

    let attacker = reqwest::Client::builder().no_proxy().build().unwrap();
    for (method, target) in [
        (Method::GET, "/ui-bridge/control/snapshot"),
        (Method::GET, "/ui-bridge/control/screenshot"),
        (Method::POST, "/ui-bridge/control/snapshot"),
    ] {
        let replay = attacker
            .request(method.clone(), format!("{url}{target}"))
            .header("X-UI-Bridge-Auth", &captured)
            .send()
            .await
            .unwrap();
        assert_eq!(
            replay.status(),
            401,
            "{method} {target} accepted a captured signature"
        );
    }
}

/// A phone that requires pairing and issued this runner no token refuses the
/// connect, which says so and names the runner's pair route. A token another
/// phone issued is never used for it.
#[tokio::test]
async fn an_unpaired_phone_refuses_the_connect_with_pairing_required() {
    let _door = door();
    let phone = Arc::new(Phone::new(false));
    let other = Phone::new(false);
    device_tokens()
        .store(&other.device_id, &other.token, true)
        .unwrap();
    let (url, _forget) = phone_behind_lan_proxy(phone.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let Err(envelope) = connect_device_at(&manager, "lan-unpaired".into(), url).await else {
        panic!("an unpaired phone must refuse the connect");
    };
    assert_eq!(
        envelope.hint,
        Some(serde_json::json!({ "authPath": "pairing_required" }))
    );
    assert_eq!(envelope.code.as_deref(), Some("PAIRING_REQUIRED"));
    let message = envelope.error.unwrap_or_default();
    assert!(
        message.contains("POST /ui-bridge/devices/{id}/pair"),
        "{message}"
    );
    assert!(message.contains(&phone.device_id), "{message}");
    assert!(manager.lock().await.active_connection().is_none());
    assert!(
        phone.gated_requests().is_empty(),
        "nothing past health was sent"
    );
    assert_no_secret_crossed(&phone);
}

/// A phone whose clock is far from this machine's refuses the first signature
/// with its own `serverTime`; the request is re-signed on it once and goes
/// through.
#[tokio::test]
async fn a_phone_clock_that_moved_since_the_handshake_costs_one_resigned_retry() {
    let _door = door();
    let phone = Arc::new(Phone::new(false));
    device_tokens()
        .store(&phone.device_id, &phone.token, true)
        .unwrap();
    let (url, _forget) = phone_behind_lan_proxy(phone.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    connect_device_at(&manager, "lan-skew".into(), url.clone())
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {:?}", e.error));
    phone.skew_ms.store(3_600_000, Ordering::SeqCst);

    let client = manager
        .lock()
        .await
        .active_connection()
        .unwrap()
        .client
        .clone();
    let resp = client
        .post(format!("{url}/ui-bridge/control/element/x/action"))
        .json(&serde_json::json!({ "action": "click" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let gated = phone.gated_requests();
    let statuses: Vec<u16> = gated.iter().map(|s| s.status).collect();
    assert_eq!(statuses, vec![401, 200], "{gated:?}");
    assert_eq!(
        gated[0].body, gated[1].body,
        "the retry carries the same body"
    );
}

/// A phone that revoked this runner's pairing refuses every request, and the
/// refusal the caller reads names the runner's pair route, with the phone's own
/// text kept beside it.
#[tokio::test]
async fn a_revoked_pairing_reads_as_how_to_pair_this_runner_again() {
    let _door = door();
    let mut revoked = Phone::new(false);
    revoked.revoked = true;
    let phone = Arc::new(revoked);
    device_tokens()
        .store(&phone.device_id, &phone.token, true)
        .unwrap();
    let (url, _forget) = phone_behind_lan_proxy(phone.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    connect_device_at(&manager, "lan-revoked".into(), url.clone())
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {:?}", e.error));
    let client = manager
        .lock()
        .await
        .active_connection()
        .unwrap()
        .client
        .clone();

    let resp = client
        .get(format!("{url}/ui-bridge/control/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "PAIRING_REQUIRED");
    let error = body["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("POST /ui-bridge/devices/{id}/pair"),
        "{error}"
    );
    assert!(error.contains(&phone.device_id), "{error}");
    assert!(
        body["deviceError"]
            .as_str()
            .unwrap_or_default()
            .contains("the phone's own text"),
        "{body}"
    );
}

/// Over a cable the phone admits the runner as on the phone itself: the connect
/// is unsigned, and the token is fetched in the background with no code, so
/// the same phone is reachable over Wi-Fi once the cable is out.
#[tokio::test]
async fn a_cable_connect_is_unsigned_and_fetches_the_token_with_no_code() {
    let _door = door();
    let phone = Arc::new(Phone::new(true));
    let (url, _forget) = phone_over_a_cable(phone.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let connected = connect_sdk_app(
        &manager,
        &url,
        None,
        None,
        None,
        Some("mobile".into()),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("the connect failed: {e}"));
    assert_ne!(connected.auth_path, "device-signed");

    let store = device_tokens();
    for _ in 0..100 {
        if store.has_token(&phone.device_id) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        store.token_for(&phone.device_id).as_deref(),
        Some(phone.token.as_str())
    );
    let pairs: Vec<Seen> = phone
        .seen()
        .into_iter()
        .filter(|s| s.target == "/ui-bridge/pair")
        .collect();
    assert_eq!(pairs.len(), 1, "{pairs:?}");
    assert_eq!(pairs[0].body, b"{}", "no code is sent over a cable");
    assert_no_secret_crossed(&phone);
}

/// The pair call over Wi-Fi: no code armed, a missing code and a wrong one are
/// refused and store nothing; the code the app shows stores the token under the
/// phone's own id; an app that predates pairing answers 404.
#[tokio::test]
async fn pairing_over_wifi_needs_the_code_the_app_shows() {
    let _door = door();
    let store = DeviceTokenStore::in_memory();

    let unarmed = Arc::new(Phone::new(false));
    let (url, _forget_unarmed) = phone_behind_lan_proxy(unarmed.clone()).await;
    let err = pair_with_device(&url, &store, with_code(Some("123456")))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "PAIRING_NOT_ARMED");

    let mut armed = Phone::new(false);
    armed.code = Some("424242".into());
    let armed = Arc::new(armed);
    let (url, _forget_armed) = phone_behind_lan_proxy(armed.clone()).await;
    assert_eq!(
        pair_with_device(&url, &store, with_code(None))
            .await
            .unwrap_err()
            .code(),
        "PAIRING_CODE_REQUIRED"
    );
    assert_eq!(
        pair_with_device(&url, &store, with_code(Some("000000")))
            .await
            .unwrap_err()
            .code(),
        "PAIRING_CODE_INVALID"
    );
    assert!(!store.has_token(&armed.device_id));
    let paired = pair_with_device(&url, &store, with_code(Some("424242")))
        .await
        .unwrap();
    assert_eq!(paired.device_id, armed.device_id);
    assert_eq!(
        store.token_for(&armed.device_id).as_deref(),
        Some(armed.token.as_str())
    );
    for seen in armed.seen() {
        assert_eq!(seen.authorization, None);
    }

    let old_app = Arc::new(Phone::new(false));
    let port = spawn_phone(old_app).await;
    // Off the `/ui-bridge/` prefix, the stand-in answers 404, as an app that
    // predates pairing does at `/ui-bridge/pair`.
    let err = pair_with_device(
        &format!("http://127.0.0.1:{port}/legacy"),
        &store,
        with_code(None),
    )
    .await
    .unwrap_err();
    assert_eq!(err, PairError::AppTooOld);
}

/// A device connect passes over a transport whose phone requires pairing this
/// runner has not done and connects over the next, making it the active one.
/// Pairing then makes the LAN transport active again and drops the relay
/// connection, so the next connect goes back to the LAN, signed.
#[tokio::test]
async fn an_unpaired_phone_on_wifi_is_reached_over_its_relay_instead() {
    let _door = door();
    let mut lan_phone = Phone::new(false);
    lan_phone.code = Some("135790".into());
    let lan_phone = Arc::new(lan_phone);
    let (lan_url, _forget_lan) = phone_behind_lan_proxy(lan_phone.clone()).await;
    // The relay's path is not covered by the phone's gate; the stand-in for it
    // admits the runner as the phone admits a caller on the phone itself.
    let relay = Arc::new(Phone::new(true));
    let (relay_url, _forget_relay) = phone_behind_relay(relay.clone()).await;

    let registry = PhysicalDeviceRegistry::new();
    let info = PhysicalDeviceInfo {
        id: lan_phone.device_id.clone(),
        os: DeviceOs::Android,
        device_kind: "physical".into(),
        model: None,
        app_id: None,
        ui_bridge_version: None,
        first_seen_at: 0,
    };
    let transport = |kind, proxy_url: &str| ActiveTransport {
        kind,
        proxy_url: proxy_url.to_string(),
        established_at: 0,
        last_healthy_at: 0,
        fail_count: 0,
    };
    registry
        .register(info.clone(), transport(TransportKind::Cloud, &relay_url))
        .await;
    registry
        .register(info, transport(TransportKind::Lan, &lan_url))
        .await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let connected = connect_device_over_transports(&manager, &registry, &lan_phone.device_id)
        .await
        .unwrap_or_else(|e| panic!("no transport connected: {:?}", e.map(|e| e.error)));
    let connected = serde_json::to_value(&connected).unwrap();
    assert_eq!(connected["proxyUrl"], relay_url.as_str());
    assert_eq!(
        registry
            .get_active_proxy_url(&lan_phone.device_id)
            .await
            .as_deref(),
        Some(relay_url.as_str())
    );
    assert!(lan_phone.gated_requests().is_empty());

    let paired = pair_device_with(
        &manager,
        &registry,
        &device_tokens(),
        lan_phone.device_id.clone(),
        Some("135790".into()),
        false,
    )
    .await
    .unwrap_or_else(|(status, e)| panic!("the pair failed ({status}): {:?}", e.error));
    let paired = serde_json::to_value(&paired).unwrap();
    assert_eq!(paired["transport"], "lan");
    assert_eq!(paired["uiBridgeDeviceId"], lan_phone.device_id.as_str());
    assert_eq!(
        registry
            .get_active_proxy_url(&lan_phone.device_id)
            .await
            .as_deref(),
        Some(lan_url.as_str()),
        "pairing makes the paired transport the active one again"
    );
    {
        let manager = manager.lock().await;
        assert!(
            manager.active_connection().is_none(),
            "the relay connection was dropped"
        );
        assert!(!manager.connections.contains_key(&relay_url));
    }
    let reconnected = connect_device_over_transports(&manager, &registry, &lan_phone.device_id)
        .await
        .unwrap_or_else(|e| panic!("no transport connected: {:?}", e.map(|e| e.error)));
    let reconnected = serde_json::to_value(&reconnected).unwrap();
    assert_eq!(reconnected["proxyUrl"], lan_url.as_str());
    assert_eq!(reconnected["authPath"], "device-signed");
    let client = {
        let manager = manager.lock().await;
        manager
            .active_connection()
            .expect("the LAN is the active target")
            .client
            .clone()
    };
    let snapshot = client
        .get(format!("{lan_url}/ui-bridge/control/snapshot"))
        .send()
        .await
        .expect("a snapshot");
    assert_eq!(snapshot.status(), 200);
    let gated = lan_phone.gated_requests();
    assert_eq!(gated.len(), 1, "{gated:?}");
    assert!(
        gated[0].auth.is_some() && gated[0].status == 200,
        "{gated:?}"
    );
    assert_no_secret_crossed(&lan_phone);

    // With no fallback left, the pairing refusal is what the caller reads.
    let unpaired = Arc::new(Phone::new(false));
    let (unpaired_url, _forget_unpaired) = phone_behind_lan_proxy(unpaired.clone()).await;
    let lonely = PhysicalDeviceRegistry::new();
    let info = PhysicalDeviceInfo {
        id: "lan-only".into(),
        os: DeviceOs::Android,
        device_kind: "physical".into(),
        model: None,
        app_id: None,
        ui_bridge_version: None,
        first_seen_at: 0,
    };
    lonely
        .register(info, transport(TransportKind::Lan, &unpaired_url))
        .await;
    let refusal = connect_device_over_transports(&manager, &lonely, "lan-only")
        .await
        .unwrap_err()
        .expect("a transport was tried");
    assert_eq!(refusal.code.as_deref(), Some("PAIRING_REQUIRED"));
    assert!(connect_device_over_transports(&manager, &lonely, "ghost")
        .await
        .unwrap_err()
        .is_none());
}

/// Wait long enough for a background token fetch to have run, had one started.
async fn settle_background_fetch() {
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
}

/// A server behind a LAN proxy that answers as a phone admitting this runner as
/// on the phone itself is not taken at its word: no token is fetched from it,
/// in the background, over a fresh forward, or on the operator's code-less pair,
/// because anything on the network can answer that way. Only a cable is a cable.
#[tokio::test]
async fn a_lan_server_claiming_to_be_local_gets_no_token_fetch() {
    let _door = door();
    let impostor = Arc::new(Phone::new(true));
    let (url, _forget) = phone_behind_lan_proxy(impostor.clone()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    connect_sdk_app(
        &manager,
        &url,
        None,
        None,
        None,
        Some("mobile".into()),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("the connect failed: {e}"));
    settle_background_fetch().await;
    assert_eq!(pair_over_local_forward(&url).await.status, "not_cable");

    let registry = PhysicalDeviceRegistry::new();
    registry
        .register(
            PhysicalDeviceInfo {
                id: impostor.device_id.clone(),
                os: DeviceOs::Android,
                device_kind: "physical".into(),
                model: None,
                app_id: None,
                ui_bridge_version: None,
                first_seen_at: 0,
            },
            ActiveTransport {
                kind: TransportKind::Lan,
                proxy_url: url.clone(),
                established_at: 0,
                last_healthy_at: 0,
                fail_count: 0,
            },
        )
        .await;
    let Err((status, refusal)) = pair_device_with(
        &manager,
        &registry,
        &device_tokens(),
        impostor.device_id.clone(),
        None,
        false,
    )
    .await
    else {
        panic!("a code-less pair over the LAN was accepted");
    };
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refusal.code.as_deref(), Some("PAIRING_CODE_REQUIRED"));

    assert!(
        impostor
            .seen()
            .iter()
            .all(|s| s.target != "/ui-bridge/pair"),
        "a pair was sent to a server that is not a cable: {:?}",
        impostor.seen()
    );
    assert!(!device_tokens().has_token(&impostor.device_id));
    assert_no_secret_crossed(&impostor);
}

/// A forward the adb server lists is a cable only for a device on USB. Over an
/// `adb connect` to a phone on Wi-Fi the phone admits the forward as local too, but
/// legacy adb over TCP is neither encrypted nor checked, so what answers is whatever
/// holds that address on the LAN. A code-less pair there is refused before anything
/// is sent, and the same forward to a USB serial pairs with no code.
#[tokio::test]
async fn only_an_adb_forward_to_a_usb_device_is_a_cable() {
    let _door = door();
    let phone = Arc::new(Phone::new(true));
    let port = spawn_phone(phone.clone()).await;
    let url = format!("http://127.0.0.1:{port}");
    let registry = PhysicalDeviceRegistry::new();
    registry
        .register(
            PhysicalDeviceInfo {
                id: phone.device_id.clone(),
                os: DeviceOs::Android,
                device_kind: "physical".into(),
                model: None,
                app_id: None,
                ui_bridge_version: None,
                first_seen_at: 0,
            },
            ActiveTransport {
                kind: TransportKind::Usb,
                proxy_url: url.clone(),
                established_at: 0,
                last_healthy_at: 0,
                fail_count: 0,
            },
        )
        .await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let tokens = device_tokens();
    let pair = || {
        pair_device_with(
            &manager,
            &registry,
            &tokens,
            phone.device_id.clone(),
            None,
            false,
        )
    };

    for network_serial in ["192.168.1.50:5555", "adb-R3CN30-x1._adb-tls-connect._tcp"] {
        let _adb = FakeAdb::start(format!("{network_serial} tcp:{port} tcp:9876\n")).await;
        let Err((status, refusal)) = pair().await else {
            panic!("a code-less pair over {network_serial} was accepted");
        };
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{network_serial}");
        assert_eq!(
            refusal.code.as_deref(),
            Some("PAIRING_CODE_REQUIRED"),
            "{network_serial}"
        );
    }
    assert!(
        phone.seen().iter().all(|s| s.target != "/ui-bridge/pair"),
        "a pair was sent over a network adb forward: {:?}",
        phone.seen()
    );
    assert!(!tokens.has_token(&phone.device_id));

    let _adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
    pair()
        .await
        .unwrap_or_else(|(status, e)| panic!("the USB pair failed ({status}): {:?}", e.error));
    assert_eq!(
        tokens.token_for(&phone.device_id).as_deref(),
        Some(phone.token.as_str())
    );
    assert_no_secret_crossed(&phone);
}

/// A pair answer must name the `deviceId` the phone's own health names. One that
/// names another phone's id, or that comes from a server whose health names none,
/// is refused, and the other phone's stored token is untouched.
#[tokio::test]
async fn a_pair_answer_naming_another_phone_keeps_nothing() {
    let _door = door();
    let store = DeviceTokenStore::in_memory();
    let victim = Phone::new(false);
    store.store(&victim.device_id, &victim.token, true).unwrap();

    let mut impostor = Phone::new(true);
    impostor.pair_device_id = Some(victim.device_id.clone());
    let impostor = Arc::new(impostor);
    let (url, _forget) = phone_over_a_cable(impostor.clone()).await;

    for expected in [None, Some(impostor.device_id.as_str())] {
        let options = PairOptions {
            expected_device_id: expected,
            may_replace: true,
            ..PairOptions::default()
        };
        let err = pair_with_device(&url, &store, options).await.unwrap_err();
        assert_eq!(err.code(), "PAIRING_BAD_ANSWER", "{expected:?}: {err:?}");
    }
    assert_eq!(
        pair_over_local_forward(&url).await.code.as_deref(),
        Some("PAIRING_BAD_ANSWER")
    );
    assert_eq!(
        store.token_for(&victim.device_id).as_deref(),
        Some(victim.token.as_str())
    );
    assert!(!store.has_token(&impostor.device_id));
    assert_eq!(
        device_tokens().token_for(&victim.device_id),
        None,
        "nothing was kept in the runner's own store either"
    );

    // A server whose health names no deviceId: there is no id to keep a token under.
    let nameless_port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new()
            .route(
                "/ui-bridge/health",
                axum::routing::get(|| async {
                    axum::Json(serde_json::json!({
                        "success": true,
                        "data": { "pairing": { "required": false, "authenticated": true } },
                    }))
                }),
            )
            .route(
                "/ui-bridge/pair",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "success": true,
                        "data": { "token": "t".repeat(64), "deviceId": "anyone", "serverTime": 0 },
                    }))
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        port
    };
    let err = pair_with_device(
        &format!("http://127.0.0.1:{nameless_port}"),
        &store,
        PairOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "PAIRING_BAD_ANSWER");
    assert!(!store.has_token("anyone"));
}

/// The dispatcher's responsiveness probe reads the phone's open health unsigned,
/// so it spends no nonce, while the request it guards is signed and accepted.
#[tokio::test]
async fn a_dispatch_probes_health_unsigned_and_signs_the_request() {
    use crate::mcp::app_dispatch::AppDispatcher;
    use crate::mcp::app_registry::AppRegistry;
    use crate::mcp::command_relay::CommandRelay;
    use crate::mcp::ws_relay::WsConnectionManager;

    let _door = door();
    let phone = Arc::new(Phone::new(false));
    device_tokens()
        .store(&phone.device_id, &phone.token, true)
        .unwrap();
    let (url, _forget) = phone_behind_lan_proxy(phone.clone()).await;
    let manager = Arc::new(tokio::sync::Mutex::new(SdkConnectionManager::new()));
    connect_device_at(&manager, "lan-dispatch".into(), url)
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {:?}", e.error));
    let before = phone.seen().len();

    let dispatcher = AppDispatcher::new(
        AppRegistry::new(),
        CommandRelay::new(WsConnectionManager::new()),
    );
    let answer = dispatcher
        .dispatch_active_http(
            &manager,
            reqwest::Method::GET,
            "/control/snapshot",
            serde_json::Value::Null,
        )
        .await
        .unwrap_or_else(|e| panic!("the dispatch failed: {e:?}"));
    assert_eq!(answer["success"], true, "{answer}");

    let after: Vec<Seen> = phone.seen().into_iter().skip(before).collect();
    let probes: Vec<&Seen> = after
        .iter()
        .filter(|s| s.target.starts_with("/ui-bridge/health"))
        .collect();
    assert!(
        !probes.is_empty(),
        "the dispatcher probed health: {after:?}"
    );
    assert!(
        probes.iter().all(|s| s.auth.is_none()),
        "health was signed: {probes:?}"
    );
    let gated: Vec<&Seen> = after
        .iter()
        .filter(|s| s.target.starts_with("/ui-bridge/control/snapshot"))
        .collect();
    assert_eq!(gated.len(), 1, "{after:?}");
    assert!(
        gated[0].auth.is_some() && gated[0].status == 200,
        "{gated:?}"
    );
    assert_no_secret_crossed(&phone);
}

/// A server on this machine's loopback that no forward of this runner's names
/// (a local emulator) is dialled as a local relay, which would carry the
/// operator's bearer. Its health is asked first with no bearer, and it answers as
/// a phone, so no request it sees carries `Authorization`, the handshake's
/// included, and no token is fetched, since it is not a cable.
#[tokio::test]
async fn a_phone_dialled_as_a_local_relay_is_never_sent_the_bearer() {
    let _door = door();
    let emulator = Arc::new(Phone::new(true));
    let port = spawn_phone(emulator.clone()).await;
    let url = format!("http://127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let connected = connect_sdk_app(
        &manager,
        &url,
        None,
        None,
        None,
        Some("mobile".into()),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("the connect failed: {e}"));
    assert_eq!(connected.auth_path, "withheld_from_device");
    let handshake = emulator.seen().len();
    assert!(handshake > 0, "the handshake reached the phone");

    let client = manager
        .lock()
        .await
        .active_connection()
        .unwrap()
        .client
        .clone();
    for path in ["/ui-bridge/control/snapshot", "/ui-bridge/health"] {
        let resp = client.get(format!("{url}{path}")).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
    }
    recheck_stored_bearers(&manager).await;
    let resp = client
        .get(format!("{url}/ui-bridge/control/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    settle_background_fetch().await;

    let seen = emulator.seen();
    assert!(seen.len() >= handshake + 3, "{seen:?}");
    assert!(
        seen.iter().all(|s| s.authorization.is_none()),
        "the bearer reached the phone: {seen:?}"
    );
    assert!(seen.iter().all(|s| s.target != "/ui-bridge/pair"));
    assert!(!device_tokens().has_token(&emulator.device_id));
}

/// A phone connected while the adb list could not be read is held back from the
/// bearer for want of the list. Once the list reads, the re-check that gives such
/// a connection the bearer must pass it over: it answered as a phone.
#[tokio::test]
async fn a_phone_connected_while_the_adb_list_was_unreadable_never_gets_the_bearer() {
    // `door()` holds the env lock too, but from another file, where
    // `env_write_lock_guard` cannot see it. The lock is reentrant per thread.
    let _env = crate::test_env::env_lock();
    let _door = door();
    let emulator = Arc::new(Phone::new(true));
    let port = spawn_phone(emulator.clone()).await;
    let url = format!("http://127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
    start_unreadable_adb().await;
    let connected = connect_sdk_app(
        &manager,
        &url,
        None,
        None,
        None,
        Some("mobile".into()),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("the connect failed: {e}"));
    assert_eq!(connected.auth_path, "withheld_forward_list_unreadable");
    drop(restore);

    recheck_stored_bearers(&manager).await;
    let client = manager
        .lock()
        .await
        .active_connection()
        .unwrap()
        .client
        .clone();
    let resp = client
        .get(format!("{url}/ui-bridge/control/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let seen = emulator.seen();
    assert!(seen
        .iter()
        .any(|s| s.target.starts_with("/ui-bridge/control/snapshot")));
    assert!(
        seen.iter().all(|s| s.authorization.is_none()),
        "the bearer reached the phone: {seen:?}"
    );
}

/// A phone that requires pairing, dialled as a local relay (an `iproxy` or `ssh -L`
/// this runner cannot see), is signed once paired rather than refused, and never
/// sent the bearer.
#[tokio::test]
async fn a_paired_phone_dialled_as_a_local_relay_is_signed() {
    let _door = door();
    let phone = Arc::new(Phone::new(false));
    let port = spawn_phone(phone.clone()).await;
    let url = format!("http://127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let Err(refused) = connect_sdk_app(
        &manager,
        &url,
        None,
        None,
        None,
        Some("mobile".into()),
        None,
    )
    .await
    else {
        panic!("an unpaired phone must refuse the connect");
    };
    assert_eq!(refused.auth_path, "pairing_required");

    device_tokens()
        .store(&phone.device_id, &phone.token, true)
        .unwrap();
    let connected = connect_sdk_app(
        &manager,
        &url,
        None,
        None,
        None,
        Some("mobile".into()),
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("the connect failed: {e}"));
    assert_eq!(connected.auth_path, "device-signed");
    let client = manager
        .lock()
        .await
        .active_connection()
        .unwrap()
        .client
        .clone();
    let resp = client
        .get(format!("{url}/ui-bridge/control/snapshot"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let gated = phone.gated_requests();
    assert!(gated.iter().all(|s| s.auth.is_some()), "{gated:?}");
    assert_no_secret_crossed(&phone);
}

/// A phone hands every pairing the same token until the app revokes it, so a pair
/// answer with a different token for a phone this runner already holds one for is
/// either a revoke or a device answering as that phone. Off a cable it is kept only
/// when the operator says the pairing was revoked (`replace`).
#[tokio::test]
async fn a_different_token_replaces_a_held_one_only_when_told_to() {
    let _door = door();
    let victim = Phone::new(false);
    device_tokens()
        .store(&victim.device_id, &victim.token, true)
        .unwrap();

    let mut impostor = Phone::new(false);
    impostor.device_id = victim.device_id.clone();
    impostor.code = Some("246810".into());
    let impostor = Arc::new(impostor);
    let (url, _forget) = phone_behind_lan_proxy(impostor.clone()).await;
    let registry = PhysicalDeviceRegistry::new();
    registry
        .register(
            PhysicalDeviceInfo {
                id: victim.device_id.clone(),
                os: DeviceOs::Android,
                device_kind: "physical".into(),
                model: None,
                app_id: None,
                ui_bridge_version: None,
                first_seen_at: 0,
            },
            ActiveTransport {
                kind: TransportKind::Lan,
                proxy_url: url,
                established_at: 0,
                last_healthy_at: 0,
                fail_count: 0,
            },
        )
        .await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let tokens = device_tokens();
    let pair = |replace| {
        pair_device_with(
            &manager,
            &registry,
            &tokens,
            victim.device_id.clone(),
            Some("246810".into()),
            replace,
        )
    };

    let Err((status, refusal)) = pair(false).await else {
        panic!("a different token replaced the held one");
    };
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refusal.code.as_deref(), Some("PAIRING_TOKEN_HELD"));
    assert_eq!(
        device_tokens().token_for(&victim.device_id).as_deref(),
        Some(victim.token.as_str())
    );

    pair(true)
        .await
        .unwrap_or_else(|(status, e)| panic!("replace failed ({status}): {:?}", e.error));
    assert_eq!(
        device_tokens().token_for(&victim.device_id).as_deref(),
        Some(impostor.token.as_str())
    );
}
