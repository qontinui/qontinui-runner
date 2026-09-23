//! Acceptance and legitimate-flow tests for plan
//! `2026-09-17-ui-bridge-relay-registration-is-unauthenticated`.
//!
//! # Harness
//!
//! Every test spawns its own server on an ephemeral loopback port: the REAL
//! relay handlers (taking `State<RelayState>`), registered at their REAL route
//! patterns, behind the REAL origin guard (`origin_guard::apply`, default route
//! policy). Clients are real too — `tokio-tungstenite` for `/ui-bridge/ws`,
//! `reqwest` for the HTTP routes and the SSE command stream — and set `Origin`,
//! `Sec-Fetch-Site` and `X-UI-Bridge-Tab-Key` explicitly, so a request reaches
//! a handler exactly the way the captured browser shapes do (Phase 0 step 1).
//! Each test builds its own [`RelayState`] with an explicit [`BindingConfig`];
//! **no test sets an environment variable**, and no counter is process-global.
//!
//! # Two kinds of test
//!
//! - **Acceptance tests** assert the SECURE behaviour and are `#[ignore]`d red
//!   until the phase that implements their rule. On this branch
//!   `cargo-guard.sh test relay_binding -- --ignored` FAILS, which is the proof
//!   each one checks the property it names.
//! - **Legitimate-flow tests** are the autonomy invariant: green on
//!   `origin/main` and after every phase.

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::*;
use crate::mcp::app_registry::REGISTRATION_TTL_MS;
use crate::mcp::origin_guard::{self, NormOrigin, OriginGuard};
use crate::mcp::relay_binding::BINDING_TOMBSTONE_MS;

const GOOD: &str = "https://good.example";
const EVIL: &str = "https://evil.example";
const INJECT: &str = "https://inject.example";
const APP: &str = "https://app.example";
const ACCOUNTS: &str = "https://accounts.example";
const WEB_DEV: &str = "http://localhost:3001";

/// How long a test waits for something that should happen promptly.
const PROMPT: Duration = Duration::from_secs(5);
/// How long a test waits to be sure something did NOT happen.
const QUIET: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

struct Server {
    port: u16,
    relay: RelayState,
    http: reqwest::Client,
}

/// A server under the runner's DEFAULT route policy.
async fn spawn(config: BindingConfig) -> Server {
    spawn_with_policy(None, config).await
}

/// A server under an explicit route policy (`None` = the default,
/// `EnforceDoors`). R7 claims its refusal holds in EVERY route policy, so its
/// test exercises more than one.
async fn spawn_with_policy(policy: Option<&str>, config: BindingConfig) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let bound = Arc::new(AtomicU16::new(port));
    let guard = Arc::new(OriginGuard::new(
        None,
        policy,
        None,
        None,
        Arc::new(move || bound.load(Ordering::Relaxed)),
        Arc::new(|| Arc::new(Vec::<NormOrigin>::new())),
    ));
    let relay = RelayState::standalone(config);
    let router: Router = relay_router(relay.clone());
    let router = origin_guard::apply(router, guard);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Server {
        port,
        relay,
        http: reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()
            .unwrap(),
    }
}

/// The relay routes at their production patterns, over `RelayState`.
///
/// `/test-health` is the HARNESS's own counter read — deliberately NOT spelled
/// `/health`, because the production handler needs an `ApiState` (and so a
/// `tauri::AppHandle`) no test can build. Phase 1 DID wire
/// `RelayBinding::health_json` into the production `/health`; what pins that
/// is `mcp_api::ui_bridge_binding_health_tests`, which asserts the block's
/// render through `health_json` and its wiring against the handler's own
/// source. Nothing HERE is evidence an operator can see a counter.
fn relay_router(relay: RelayState) -> Router {
    use crate::mcp::app_discovery::{
        deregister_app, dispatch_to_app, list_registered_apps, register_app,
    };
    use crate::mcp::sdk_client::{handle_connections, handle_switch};
    use crate::mcp::ui_bridge::relay::{
        ui_bridge_relay_command_result_handler, ui_bridge_relay_command_stream_handler,
        ui_bridge_relay_dispatch_handler, ui_bridge_relay_heartbeat_handler,
        ui_bridge_relay_tabs_handler,
    };
    use crate::mcp::ws_relay::ws_upgrade_handler;
    Router::new()
        .route("/ui-bridge/ws", any(ws_upgrade_handler))
        .route("/ui-bridge/apps/register", post(register_app))
        .route("/ui-bridge/apps/register/{app_id}", delete(deregister_app))
        .route("/ui-bridge/apps/registered", get(list_registered_apps))
        .route("/ui-bridge/apps/{app_id}/dispatch", post(dispatch_to_app))
        .route("/ui-bridge/sdk/switch", post(handle_switch))
        .route("/ui-bridge/sdk/connections", get(handle_connections))
        .route(
            "/ui-bridge/commands/stream",
            get(ui_bridge_relay_command_stream_handler),
        )
        .route(
            "/ui-bridge/commands",
            post(ui_bridge_relay_command_result_handler),
        )
        .route(
            "/ui-bridge/heartbeat",
            post(ui_bridge_relay_heartbeat_handler),
        )
        .route("/ui-bridge/tabs", get(ui_bridge_relay_tabs_handler))
        .route(
            "/ui-bridge/relay/dispatch",
            post(ui_bridge_relay_dispatch_handler),
        )
        .route(
            "/test-health",
            get(|State(s): State<RelayState>| async move {
                Json(json!({ "uiBridgeBinding": s.binding.health_json() }))
            }),
        )
        .with_state(relay)
}

/// Request headers a caller sends. `origin: None` is an agent / script.
#[derive(Clone, Copy, Default)]
struct Caller<'a> {
    origin: Option<&'a str>,
    tab_key: Option<&'a str>,
}

fn agent() -> Caller<'static> {
    Caller::default()
}

fn browser(origin: &str) -> Caller<'_> {
    Caller {
        origin: Some(origin),
        tab_key: None,
    }
}

fn keyed<'a>(origin: &'a str, key: &'a str) -> Caller<'a> {
    Caller {
        origin: Some(origin),
        tab_key: Some(key),
    }
}

impl Server {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        from: Caller<'_>,
    ) -> reqwest::RequestBuilder {
        let mut b = self.http.request(method, self.url(path));
        if let Some(o) = from.origin {
            // A cross-origin `cors` fetch, as captured from Chromium.
            b = b.header("origin", o).header("sec-fetch-site", "cross-site");
        }
        if let Some(k) = from.tab_key {
            b = b.header("x-ui-bridge-tab-key", k);
        }
        b
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        from: Caller<'_>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut b = self.request(method, path, from);
        if let Some(body) = body {
            b = b.json(&body);
        }
        let resp = tokio::time::timeout(Duration::from_secs(30), b.send())
            .await
            .expect("response within 30s")
            .expect("request sent");
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn post(&self, path: &str, from: Caller<'_>, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, from, Some(body))
            .await
    }

    async fn get(&self, path: &str, from: Caller<'_>) -> (u16, Value) {
        self.call(reqwest::Method::GET, path, from, None).await
    }

    async fn delete(&self, path: &str, from: Caller<'_>) -> (u16, Value) {
        self.call(reqwest::Method::DELETE, path, from, None).await
    }

    // -- WebSocket wrappers ----------------------------------------------

    async fn ws_open(&self, origin: Option<&str>) -> Ws {
        let mut req = format!("ws://127.0.0.1:{}/ui-bridge/ws", self.port)
            .into_client_request()
            .unwrap();
        if let Some(o) = origin {
            req.headers_mut()
                .insert("origin", HeaderValue::from_str(o).unwrap());
        }
        let (stream, resp) = tokio::time::timeout(PROMPT, tokio_tungstenite::connect_async(req))
            .await
            .expect("upgrade within 5s")
            .expect("upgrade accepted");
        assert_eq!(resp.status().as_u16(), 101, "upgrade status");
        Ws { stream }
    }

    /// Open a socket from `origin` and send the register frame the extension's
    /// live relay / a Node wrapper sends. Returns the socket and the runner's
    /// first reply frame.
    async fn ws_register(&self, origin: Option<&str>, app_id: &str) -> (Ws, Value) {
        let mut ws = self.ws_open(origin).await;
        let mut frame = json!({
            "type": "register",
            "transport": "websocket",
            "appId": app_id,
            "appName": format!("{app_id} (test)"),
            "appType": "wrapper",
        });
        if let Some(o) = origin {
            frame["origin"] = json!(o);
            frame["pageUrl"] = json!(format!("{o}/page"));
        }
        ws.send(frame).await;
        let reply = ws.recv().await.expect("a reply to the register frame");
        (ws, reply)
    }

    // -- SSE relay tabs ----------------------------------------------------

    /// `GET /ui-bridge/commands/stream?tabId=`. `Ok(tab)` once the stream is
    /// open and its `connected` event arrived; `Err((status, body))` when the
    /// runner answered with a non-2xx before any SSE byte.
    async fn attach(&self, tab_id: &str, from: Caller<'_>) -> Result<Tab, (u16, Value)> {
        let path = format!("/ui-bridge/commands/stream?tabId={tab_id}");
        let resp = tokio::time::timeout(
            PROMPT,
            self.request(reqwest::Method::GET, &path, from)
                .header("accept", "text/event-stream")
                .send(),
        )
        .await
        .expect("stream response head within 5s")
        .expect("stream request sent");
        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err((
                status,
                serde_json::from_str(&text).unwrap_or(Value::String(text)),
            ));
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut body = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(Ok(chunk)) = body.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(end) = buf.find("\n\n") {
                    let block: String = buf.drain(..end + 2).collect();
                    let data: Vec<&str> = block
                        .lines()
                        .filter_map(|l| l.strip_prefix("data:"))
                        .map(|d| d.strip_prefix(' ').unwrap_or(d))
                        .collect();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) {
                        if tx.send(v).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        let mut tab = Tab { rx, task };
        let first = tab
            .next(PROMPT)
            .await
            .expect("the stream's connected event");
        assert_eq!(first["type"], "connected", "first SSE event: {first}");
        Ok(tab)
    }

    /// `POST /ui-bridge/heartbeat` for `tab_id` with a page `url`.
    async fn heartbeat(&self, tab_id: &str, from: Caller<'_>, url: &str) -> (u16, Value) {
        self.post(
            "/ui-bridge/heartbeat",
            from,
            json!({ "tabId": tab_id, "url": url, "title": "t", "appType": "injected" }),
        )
        .await
    }

    /// An agent's `POST /ui-bridge/relay/dispatch`, run in the background.
    fn relay_dispatch(
        &self,
        tab_id: Option<&str>,
        timeout_ms: u64,
    ) -> tokio::task::JoinHandle<(u16, Value)> {
        let http = self.http.clone();
        let url = self.url("/ui-bridge/relay/dispatch");
        let mut body =
            json!({ "action": "getControlSnapshot", "payload": {}, "timeoutMs": timeout_ms });
        if let Some(t) = tab_id {
            body["tabId"] = json!(t);
        }
        tokio::spawn(async move {
            let resp = http
                .post(url)
                .json(&body)
                .send()
                .await
                .expect("dispatch sent");
            let status = resp.status().as_u16();
            (status, resp.json().await.unwrap_or(Value::Null))
        })
    }

    /// An agent's `POST /ui-bridge/apps/{app_id}/dispatch`, run in the
    /// background.
    fn app_dispatch(&self, app_id: &str) -> tokio::task::JoinHandle<(u16, Value)> {
        let http = self.http.clone();
        let url = self.url(&format!("/ui-bridge/apps/{app_id}/dispatch"));
        tokio::spawn(async move {
            let resp = http
                .post(url)
                .json(&json!({ "action": "getControlSnapshot", "params": {} }))
                .send()
                .await
                .expect("dispatch sent");
            let status = resp.status().as_u16();
            (status, resp.json().await.unwrap_or(Value::Null))
        })
    }

    /// Post a tab's result envelope for `command_id`.
    async fn post_result(
        &self,
        from: Caller<'_>,
        tab_id: &str,
        command_id: &str,
        result: Value,
    ) -> (u16, Value) {
        self.post(
            "/ui-bridge/commands",
            from,
            json!({ "commandId": command_id, "tabId": tab_id, "success": true, "result": result }),
        )
        .await
    }

    async fn tab_entry(&self, tab_id: &str) -> Option<Value> {
        let (_, body) = self.get("/ui-bridge/tabs", agent()).await;
        body["data"]["tabs"]
            .as_array()
            .and_then(|tabs| tabs.iter().find(|t| t["tabId"] == tab_id).cloned())
    }

    /// Wait until the runner reports `tab_id` as having no live stream.
    ///
    /// A dropped SSE client is noticed on the runner's next write to it, and
    /// the only write an idle stream gets is the 15 s keep-alive; a targeted
    /// dispatch with a short timeout is a write, so this nudges with those.
    async fn wait_tab_disconnected(&self, tab_id: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(t) = self.tab_entry(tab_id).await {
                if t["connected"] == false {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "harness: the runner never noticed tab {tab_id}'s stream drop"
            );
            let _ = self.relay_dispatch(Some(tab_id), 50).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn registered(&self, app_id: &str) -> Option<Value> {
        let (_, body) = self.get("/ui-bridge/apps/registered", agent()).await;
        body["data"]
            .as_array()
            .and_then(|apps| apps.iter().find(|a| a["appId"] == app_id).cloned())
    }

    async fn active_app_id(&self) -> Option<String> {
        let mgr = self.relay.sdk_connection.lock().await;
        mgr.active_connection().map(|c| c.app_info.app_id.clone())
    }

    async fn active_url(&self) -> Option<String> {
        self.relay.sdk_connection.lock().await.active_url.clone()
    }

    /// A rule's counters, read from the HARNESS stub — see `relay_router`.
    /// The production `/health` serves the same block from the ONE shared
    /// `RelayBinding`; `mcp_api::ui_bridge_binding_health_tests` pins that.
    async fn rule(&self, rule: &str) -> Value {
        let (_, body) = self.get("/test-health", agent()).await;
        body["uiBridgeBinding"]["rules"][rule].clone()
    }
}

struct Ws {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Ws {
    async fn send(&mut self, frame: Value) {
        self.stream
            .send(WsMessage::Text(frame.to_string().into()))
            .await
            .expect("frame sent");
    }

    /// Next text frame as JSON, `None` on close or after [`PROMPT`].
    async fn recv(&mut self) -> Option<Value> {
        self.recv_within(PROMPT).await
    }

    async fn recv_within(&mut self, within: Duration) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.stream.next()).await {
                Ok(Some(Ok(WsMessage::Text(t)))) => {
                    return serde_json::from_str(t.as_str()).ok();
                }
                Ok(Some(Ok(WsMessage::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                    return None
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    /// Answer a command frame the way a wrapper does.
    async fn respond(&mut self, command_id: &str, result: Value) {
        self.send(json!({
            "type": "response",
            "commandId": command_id,
            "success": true,
            "result": result,
        }))
        .await;
    }

    async fn close(mut self) {
        let _ = self.stream.close(None).await;
    }
}

struct Tab {
    rx: tokio::sync::mpsc::UnboundedReceiver<Value>,
    task: tokio::task::JoinHandle<()>,
}

impl Tab {
    async fn next(&mut self, within: Duration) -> Option<Value> {
        tokio::time::timeout(within, self.rx.recv())
            .await
            .ok()
            .flatten()
    }

    /// Drop the stream without a goodbye, the way a navigation does.
    fn drop_stream(self) {
        self.task.abort();
    }
}

fn code(body: &Value) -> Option<&str> {
    body["code"]
        .as_str()
        .or_else(|| body["error"]["code"].as_str())
}

/// The `ack ok:false` refusal frame's code.
fn ack_refusal_code(frame: &Value) -> Option<&str> {
    (frame["type"] == "ack" && frame["ok"] == false)
        .then(|| frame["error"]["code"].as_str())
        .flatten()
}

async fn conn_for(server: &Server, app_id: &str) -> Option<u64> {
    server
        .relay
        .ws_connection_manager
        .conn_for_app(app_id)
        .await
}

fn enforce_all() -> BindingConfig {
    BindingConfig {
        binding: BindingMode::Enforce,
        active_binding: BindingMode::Enforce,
    }
}

// ===========================================================================
// Acceptance tests — SECURE behaviour, red until their phase
// ===========================================================================

/// Vector 1. Un-ignored in Phase 1, minus the `active_url` assertion, which
/// is R6 and lands in Phase 3.
#[tokio::test]
async fn hijack_live_ws_connection_refused() {
    let s = spawn(BindingConfig::default()).await;
    let (mut good, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered", "holder ack: {ack}");
    let holder_conn = ack["connId"].as_u64().unwrap();
    let active_before = s.active_url().await;

    // An agent dispatch the holder answers slowly.
    let dispatch = s.app_dispatch("app");
    let cmd = good.recv().await.expect("holder receives the command");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();

    let (_evil, reply) = s.ws_register(Some(EVIL), "app").await;
    assert_eq!(
        ack_refusal_code(&reply),
        Some("UIB_REGISTRATION_HELD"),
        "the attacker's register must be refused, got {reply}"
    );

    good.respond(&command_id, json!({ "from": "holder" })).await;
    let (status, body) = dispatch.await.unwrap();
    assert_eq!(
        status, 200,
        "in-flight dispatch must not be Displaced: {body}"
    );
    assert_eq!(body["data"]["from"], "holder");

    assert_eq!(conn_for(&s, "app").await, Some(holder_conn));
    // This is NOT the R6 shadow caveat: R6 is about a DIFFERENT appId
    // becoming active (`new_app_id_does_not_steal_active_connection`), and
    // this attacker registers the SAME id, which R1 refuses inside
    // `registry.claim` — BEFORE `install_ws_sdk_connection` runs. So the
    // assertion pins a real Phase 1 property: a refused claim touches no
    // state, and the SDK install is ordered AFTER the claim. It is the only
    // test that would catch someone hoisting the install above the claim.
    assert_eq!(s.active_url().await, active_before, "active_url moved (R1)");
    let entry = s.registered("app").await.expect("holder entry");
    assert_eq!(entry["verifiedOrigin"], GOOD);
}

/// Vector 2 (R6, enforce).
#[tokio::test]
#[ignore = "red until Phase 1/2/3 — finding 8f142485"]
async fn new_app_id_does_not_steal_active_connection() {
    let s = spawn(enforce_all()).await;
    let (_good, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    assert_eq!(s.active_app_id().await.as_deref(), Some("app"));

    let (_evil, _) = s.ws_register(Some(EVIL), "other").await;
    assert_eq!(
        s.active_app_id().await.as_deref(),
        Some("app"),
        "a foreign registration took the active connection"
    );
    assert_eq!(s.rule("R6").await["refused"], 1);
}

/// Vector 3.
#[tokio::test]
#[ignore = "red until Phase 1/2/3 — finding 8f142485"]
async fn tab_stream_takeover_refused() {
    let s = spawn(BindingConfig::default()).await;
    let mut victim = s
        .attach("t1", browser(INJECT))
        .await
        .expect("victim attaches");
    let (status, _) = s
        .heartbeat("t1", browser(INJECT), "https://inject.example/app")
        .await;
    assert_eq!(status, 200);

    match s.attach("t1", browser(EVIL)).await {
        Err((status, body)) => {
            assert_eq!(status, 409, "{body}");
            assert_eq!(code(&body), Some("UIB_REGISTRATION_HELD"), "{body}");
        }
        Ok(_) => panic!("the attacker's stream attach for t1 was admitted"),
    }

    let dispatch = s.relay_dispatch(Some("t1"), 3_000);
    let cmd = victim
        .next(PROMPT)
        .await
        .expect("the dispatch reaches the ORIGINAL stream");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();
    s.post_result(browser(INJECT), "t1", &command_id, json!({ "ok": true }))
        .await;
    assert_eq!(dispatch.await.unwrap().0, 200);

    let (status, body) = s
        .heartbeat("t1", browser(EVIL), "https://evil.example/")
        .await;
    assert!(
        status >= 400,
        "a foreign heartbeat for t1 was admitted: {body}"
    );
    let entry = s.tab_entry("t1").await.expect("t1 listed");
    assert_eq!(entry["url"], "https://inject.example/app");
}

/// Vector 4, split per transport so each arm is un-ignored in its phase: the
/// WS arm in Phase 1, the two HTTP arms in Phase 2.
mod forged_command_completion_refused {
    use super::*;

    #[tokio::test]
    #[ignore = "red until Phase 1/2/3 — finding 8f142485"]
    async fn http_foreign_origin() {
        let s = spawn(BindingConfig::default()).await;
        let mut tab = s.attach("t1", browser(INJECT)).await.expect("attach");
        let dispatch = s.relay_dispatch(Some("t1"), 5_000);
        let cmd = tab.next(PROMPT).await.expect("command delivered");
        let command_id = cmd["commandId"].as_str().unwrap().to_string();

        let (status, body) = s
            .post_result(browser(EVIL), "t1", &command_id, json!({ "forged": true }))
            .await;
        assert_eq!(status, 403, "forged completion admitted: {body}");
        assert_eq!(code(&body), Some("UIB_COMMAND_NOT_YOURS"), "{body}");
        tokio::time::sleep(QUIET).await;
        assert!(
            !dispatch.is_finished(),
            "the dispatch was completed by the forgery"
        );

        let (status, _) = s
            .post_result(
                browser(INJECT),
                "t1",
                &command_id,
                json!({ "genuine": true }),
            )
            .await;
        assert_eq!(status, 200);
        let (status, body) = dispatch.await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["data"]["result"]["genuine"], true);
    }

    #[tokio::test]
    #[ignore = "red until Phase 1/2/3 — finding 8f142485"]
    async fn http_same_origin_wrong_tab() {
        let s = spawn(BindingConfig::default()).await;
        let mut t1 = s.attach("t1", browser(INJECT)).await.expect("attach t1");
        let _t2 = s.attach("t2", browser(INJECT)).await.expect("attach t2");
        let dispatch = s.relay_dispatch(Some("t1"), 5_000);
        let cmd = t1.next(PROMPT).await.expect("command delivered");
        let command_id = cmd["commandId"].as_str().unwrap().to_string();

        let (status, body) = s
            .post_result(
                browser(INJECT),
                "t2",
                &command_id,
                json!({ "wrongTab": true }),
            )
            .await;
        assert_eq!(
            status, 403,
            "a result naming the wrong tab was admitted: {body}"
        );
        assert_eq!(code(&body), Some("UIB_COMMAND_NOT_YOURS"), "{body}");
        tokio::time::sleep(QUIET).await;
        assert!(!dispatch.is_finished());

        s.post_result(
            browser(INJECT),
            "t1",
            &command_id,
            json!({ "genuine": true }),
        )
        .await;
        assert_eq!(dispatch.await.unwrap().1["data"]["result"]["genuine"], true);
    }

    #[tokio::test]
    async fn ws_other_connection() {
        let s = spawn(BindingConfig::default()).await;
        let (mut holder, ack) = s.ws_register(Some(GOOD), "app").await;
        assert_eq!(ack["type"], "registered");
        let (mut other, ack) = s.ws_register(Some(EVIL), "other").await;
        assert_eq!(ack["type"], "registered");

        let dispatch = s.app_dispatch("app");
        let cmd = holder.recv().await.expect("holder receives the command");
        let command_id = cmd["commandId"].as_str().unwrap().to_string();

        other
            .respond(&command_id, json!({ "from": "attacker" }))
            .await;
        tokio::time::sleep(QUIET).await;
        holder
            .respond(&command_id, json!({ "from": "holder" }))
            .await;

        let (status, body) = dispatch.await.unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            body["data"]["from"], "holder",
            "the other connection's answer won"
        );
    }
}

/// Vector 5.
#[tokio::test]
async fn http_register_cannot_redirect_or_flip_transport() {
    let s = spawn(BindingConfig::default()).await;
    let (_holder, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");

    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "x", "appType": "web",
                    "transport": "http", "baseUrl": "https://evil.example" }),
        )
        .await;
    assert_eq!(
        status, 409,
        "an HTTP register overwrote a live WS holder: {body}"
    );
    let entry = s.relay.app_registry.get("app").await.expect("holder entry");
    assert_eq!(
        entry.transport,
        crate::mcp::app_registry::AppTransport::Websocket
    );

    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "fresh", "appName": "x", "appType": "web",
                    "transport": "http", "baseUrl": "https://elsewhere.example" }),
        )
        .await;
    assert_eq!(
        status, 403,
        "a baseUrl origin != header origin was admitted: {body}"
    );
    assert_eq!(code(&body), Some("UIB_ORIGIN_MISMATCH"), "{body}");
}

/// R2.
#[tokio::test]
async fn displaced_or_foreign_teardown_cannot_delete_holder() {
    let s = spawn(BindingConfig::default()).await;
    let (_holder, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");

    // A foreign connection for the same id that closes must take nothing with it.
    let (evil, _) = s.ws_register(Some(EVIL), "app").await;
    evil.close().await;
    tokio::time::sleep(QUIET).await;
    assert!(
        s.relay.app_registry.get("app").await.is_some(),
        "a foreign connection's teardown deleted the holder's entry"
    );

    let (_, body) = s
        .delete("/ui-bridge/apps/register/app", browser(EVIL))
        .await;
    assert!(
        s.relay.app_registry.get("app").await.is_some(),
        "a foreign DELETE removed the holder's entry: {body}"
    );
}

/// R5.
#[tokio::test]
async fn reload_race_reservation() {
    let s = spawn(BindingConfig::default()).await;
    let (holder, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    holder.close().await;
    // Wait for the teardown to RELEASE the id, not to delete the row: a
    // released row is retained as its holder's reservation, so it stops being
    // live (and leaves `list_live` and every routing reader) while staying
    // present for `claim` to refuse against.
    let deadline = tokio::time::Instant::now() + PROMPT;
    while s.relay.app_registry.get_live("app").await.is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "holder teardown never ran"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (_evil, reply) = s.ws_register(Some(EVIL), "app").await;
    assert_eq!(
        ack_refusal_code(&reply),
        Some("UIB_REGISTRATION_HELD"),
        "a foreign principal claimed a reserved id: {reply}"
    );

    let (_again, reply) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(
        reply["type"], "registered",
        "the same principal re-registers: {reply}"
    );
}

/// R5: a registration that ends by EXPIRY stays reserved for its holder.
///
/// `beforeunload` is not the only way a browser registration ends — a tab
/// crash, an OOM kill, a sleep, or Chrome throttling a backgrounded tab's
/// 10 s phone-home past the 30 s TTL all end it by expiry instead. Without a
/// reservation, R1 turns that into a PERMANENT lockout: an attacker claims
/// the freed id and renews it every 10 s, and the returning tab is refused
/// forever — strictly worse than main, where it simply re-took its slot. That
/// is the "one more way to be locked out" cost the plan's ranking used to
/// REJECT option A.
#[tokio::test]
async fn an_expired_registration_stays_reserved_for_its_holder() {
    let s = spawn(BindingConfig::default()).await;
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "app", "appName": "Victim", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    // The tab is backgrounded / crashed / throttled: no DELETE, no
    // phone-home, and the entry ages past its TTL.
    //
    // NO `sweep()` HERE, deliberately. The sweeper runs every 15 s while
    // `list_live` drops the row from `/ui-bridge/apps/registered` the instant
    // it expires — which is the attacker's signal — so in production the
    // attacker arrives in the skew, before any sweep. Calling `sweep()` on the
    // next line (which the first version of this test did) collapses that
    // window to zero and tests an ordering production never has.
    assert!(
        s.relay
            .app_registry
            .test_age_entry("app", REGISTRATION_TTL_MS + 1_000)
            .await
    );
    assert!(
        s.relay.app_registry.get("app").await.is_some(),
        "precondition: the row is expired but NOT yet swept — the real window"
    );
    assert!(
        s.relay
            .app_registry
            .list_live()
            .await
            .iter()
            .all(|e| e.app.app_id != "app"),
        "precondition: and it has already vanished from /ui-bridge/apps/registered"
    );

    // The attacker polling /ui-bridge/apps/registered must not get the id.
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "Squatter", "appType": "web",
                    "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
        )
        .await;
    assert_eq!(
        status, 409,
        "a swept id was claimable by a foreign page: {body}"
    );
    assert_eq!(code(&body), Some("UIB_REGISTRATION_HELD"), "{body}");

    // …and the victim coming back to the foreground is admitted, exactly as
    // it would have been on main.
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "app", "appName": "Victim", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200, "the returning holder was locked out: {body}");
    assert_eq!(
        s.registered("app").await.expect("entry")["verifiedOrigin"],
        GOOD
    );
}

/// Finding 2 / R1: the LIVE routing slot is a holder signal in its own right.
///
/// A wrapper whose send task is parked inside `sink.send().await` never
/// reaches the `heartbeat.tick()` arm that calls `touch`, so its registry row
/// ages out and is swept while the socket is wide open. R5 then reserves the
/// id for 60 s — but the socket can be parked for much longer than that, and
/// once the tombstone lapses NOTHING but the routing slot knows the holder is
/// still there. `register_with_id` would displace it.
///
/// The row is dropped through a seam rather than aged: a WebSocket client
/// auto-pongs the 20 s ping and every inbound frame refreshes `last_seen_ms`,
/// so ageing races that refresh and the test would silently fall back to
/// exercising plain R1 against a still-live row. (It did — the first version
/// of this test survived deleting the very check it names.)
#[tokio::test]
async fn a_live_ws_holder_is_not_displaceable_once_its_registry_row_is_gone() {
    let s = spawn(BindingConfig::default()).await;
    let (_holder, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    let holder_conn = ack["connId"].as_u64().unwrap();

    // The row is gone and its R5 reservation has lapsed, so the live routing
    // slot is the ONLY thing left that knows who holds this id.
    assert!(s.relay.app_registry.test_drop_row("app").await);
    assert!(
        s.relay.app_registry.get("app").await.is_none(),
        "precondition: no registry row"
    );

    let (_evil, reply) = s.ws_register(Some(EVIL), "app").await;
    assert_eq!(
        ack_refusal_code(&reply),
        Some("UIB_REGISTRATION_HELD"),
        "a foreign socket displaced a LIVE holder the registry had forgotten: {reply}"
    );
    assert_eq!(
        conn_for(&s, "app").await,
        Some(holder_conn),
        "the routing slot moved"
    );
}

/// Finding 2's second path: an HTTP phone-home for an id a WebSocket holds
/// flips the row to `Http` / `websocket_conn_id: None`. The socket's `touch`
/// must keep working after that, or the row ages out under a live socket.
#[tokio::test]
async fn ws_touch_keeps_refreshing_a_row_an_http_phone_home_took_over() {
    let s = spawn(BindingConfig::default()).await;
    let (_ws, ack) = s.ws_register(Some(GOOD), "shared").await;
    assert_eq!(ack["type"], "registered");
    let conn_id = ack["connId"].as_u64().unwrap();

    // Same principal, so R1 admits it; the row is now an HTTP entry.
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "shared", "appName": "Same origin", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        s.relay.app_registry.get("shared").await.unwrap().transport,
        crate::mcp::app_registry::AppTransport::Http
    );

    assert!(
        s.relay
            .app_registry
            .test_age_entry("shared", REGISTRATION_TTL_MS - 1_000)
            .await
    );
    let good_principal = Principal::Browser {
        class: crate::mcp::origin_guard::OriginClass::Foreign,
        origin: NormOrigin::parse(GOOD).unwrap(),
    };
    assert!(
        s.relay
            .app_registry
            .touch("shared", Some(conn_id), &good_principal)
            .await,
        "the conn guard is meaningless for an HTTP entry, and the holder's own          principal must not be blocked from refreshing it"
    );
    assert_eq!(s.relay.app_registry.sweep().await, 0);
}

/// H2 / R1-slot on the HTTP door: `POST /ui-bridge/apps/register` must not
/// displace a live WebSocket holder either.
///
/// This is the EASIER route to the same takeover — no handshake — and the one
/// that decides where agent commands go, because `AppDispatcher::dispatch`
/// reads the REGISTRY: an attacker that gets an `Http` row with its own
/// `baseUrl` receives every subsequent agent command and payload, even while
/// the victim's socket is still open. The check therefore lives inside
/// `AppRegistry::claim`, which both doors go through, rather than on the WS
/// handshake path alone.
#[tokio::test]
async fn an_http_register_cannot_displace_a_live_ws_holder_the_registry_forgot() {
    let s = spawn(BindingConfig::default()).await;
    let (_holder, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    let holder_conn = ack["connId"].as_u64().unwrap();

    // The row is gone and no reservation survives it, so the live routing
    // slot is the only thing that still knows who holds this id.
    assert!(s.relay.app_registry.test_drop_row("app").await);
    assert!(s.relay.app_registry.get("app").await.is_none());

    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "Redirect", "appType": "web",
                    "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
        )
        .await;
    assert_eq!(
        status, 409,
        "an HTTP register took the dispatch target from a live WS holder: {body}"
    );
    assert_eq!(code(&body), Some("UIB_REGISTRATION_HELD"), "{body}");
    assert!(
        s.relay.app_registry.get("app").await.is_none(),
        "the refused claim must have written nothing"
    );
    assert_eq!(conn_for(&s, "app").await, Some(holder_conn));
}

/// M1: an attacker holding an open socket on an id it first-claimed must not
/// refresh the VICTIM's row once the victim re-registers that id over HTTP.
/// First-claim squatting of an unheld id is an accepted non-goal; keeping the
/// squatted row alive against its real owner is not.
#[tokio::test]
async fn a_foreign_socket_cannot_refresh_an_http_row_it_does_not_hold() {
    let s = spawn(BindingConfig::default()).await;
    // EVIL squats the unheld id over WS and keeps the socket open.
    let (_evil, ack) = s.ws_register(Some(EVIL), "app").await;
    assert_eq!(ack["type"], "registered");
    let evil_conn = ack["connId"].as_u64().unwrap();

    // The squat lapses and GOOD takes the id over HTTP.
    assert!(s.relay.app_registry.test_drop_row("app").await);
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": "app", "appName": "Owner", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "keepAliveSecs": 30 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    let evil_principal = Principal::Browser {
        class: crate::mcp::origin_guard::OriginClass::Foreign,
        origin: NormOrigin::parse(EVIL).unwrap(),
    };
    assert!(
        !s.relay
            .app_registry
            .touch("app", Some(evil_conn), &evil_principal)
            .await,
        "a foreign socket refreshed an HTTP row it does not hold"
    );
}

/// H-1: the operator-trust carve-out on the ROW reservation.
///
/// `tombstone_until` has always exempted operator trust — an agent's id is
/// free the moment its registration ends. When the reservation moved onto the
/// row, that carve-out had to move with it, and did not. An agent registering
/// with `keepAliveSecs: 3600` and then stopping would otherwise lock a
/// legitimate browser page out of the id for 60 s past a ONE-HOUR TTL, and
/// only when the sweeper had not yet run — the same timing dependence the row
/// reservation exists to delete, sign flipped.
///
/// `agent_flow_unchanged` never covered this: it covers operator trust
/// DISPLACING, never an expired operator-trust row being re-taken.
#[tokio::test]
async fn an_expired_operator_trust_row_does_not_reserve_the_id() {
    let s = spawn(BindingConfig::default()).await;
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": "app", "appName": "Synthetic", "appType": "web",
                    "transport": "http", "baseUrl": "http://127.0.0.1:65535",
                    "keepAliveSecs": 3600 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");

    // Past its (one hour) TTL, and deliberately NOT swept.
    assert!(
        s.relay
            .app_registry
            .test_age_entry("app", 3_600_000 + 1_000)
            .await
    );
    assert!(
        s.relay.app_registry.get("app").await.is_some(),
        "precondition: the row is retained, which is what makes it a reservation"
    );

    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "app", "appName": "Page", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(
        status, 200,
        "an expired operator-trust row reserved the id against a browser: {body}"
    );
}

/// H-1's other half: the row reservation must hold for a BROWSER holder with
/// no sweep at all, which is the window the attacker actually polls.
#[tokio::test]
async fn an_expired_browser_row_reserves_the_id_with_no_sweep() {
    let s = spawn(BindingConfig::default()).await;
    let (status, _) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "app", "appName": "Victim", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(
        s.relay
            .app_registry
            .test_age_entry("app", REGISTRATION_TTL_MS + 1_000)
            .await
    );

    // No sweep. The row is retained through its reservation, so the id is
    // held even though it has already left /ui-bridge/apps/registered.
    assert!(s
        .relay
        .app_registry
        .list_live()
        .await
        .iter()
        .all(|e| e.app.app_id != "app"));
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "Squatter", "appType": "web",
                    "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
        )
        .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(code(&body), Some("UIB_REGISTRATION_HELD"), "{body}");

    // Once the reservation itself lapses, the id is free again — the accepted
    // squatting non-goal, not a permanent lock.
    assert!(
        s.relay
            .app_registry
            .test_age_entry("app", BINDING_TOMBSTONE_MS + 1_000)
            .await
    );
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "Squatter", "appType": "web",
                    "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
        )
        .await;
    assert_eq!(status, 200, "the reservation never lapsed: {body}");
}

/// The structural fix itself: `sweep` retains a row through its RESERVATION,
/// not merely its TTL, so a sweep landing inside that window leaves the id
/// held — and writes no tombstone, because the ROW is the reservation.
///
/// This is what makes R5 independent of WHEN the sweeper runs. The previous
/// shape (sweep at the TTL, then write a tombstone) left a gap the sweeper's
/// 15 s tick could not cover, while `list_live` had already dropped the row
/// from `/ui-bridge/apps/registered` — the attacker's signal — so polling at
/// 1 Hz won it roughly 14 times in 15.
#[tokio::test]
async fn a_sweep_inside_the_reservation_window_leaves_the_id_held() {
    let s = spawn(BindingConfig::default()).await;
    let (status, _) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "app", "appName": "Victim", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(
        s.relay
            .app_registry
            .test_age_entry("app", REGISTRATION_TTL_MS + 1_000)
            .await
    );

    // The sweeper runs, inside the reservation window.
    assert_eq!(
        s.relay.app_registry.sweep().await,
        0,
        "a row inside its reservation window must be retained"
    );
    assert!(
        s.relay.app_registry.get("app").await.is_some(),
        "the row IS the reservation"
    );
    assert!(
        s.relay.app_registry.get("app").await.is_some(),
        "the row IS the reservation — nothing is written anywhere else"
    );

    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "Squatter", "appType": "web",
                    "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
        )
        .await;
    assert_eq!(status, 409, "a swept row freed the id: {body}");
    assert_eq!(code(&body), Some("UIB_REGISTRATION_HELD"), "{body}");
}

/// H-1 / H-2: routing must NOT follow a row that is retained only as a
/// reservation. Retention answers "who holds this id", not "is this app
/// reachable", and conflating them would have extended the window in which a
/// dispatch targets an app that stopped heartbeating.
#[tokio::test]
async fn a_reserved_but_expired_row_is_not_dispatchable() {
    let s = spawn(BindingConfig::default()).await;
    let app = Router::new().route(
        "/dispatch",
        post(|| async { Json(json!({ "reached": true })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let (status, _) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": "stub", "appName": "Stub", "appType": "web",
                    "transport": "http",
                    "baseUrl": format!("http://127.0.0.1:{stub_port}") }),
        )
        .await;
    assert_eq!(status, 200);
    let (status, body) = s.app_dispatch("stub").await.unwrap();
    assert_eq!(status, 200, "precondition: a fresh row dispatches: {body}");

    assert!(
        s.relay
            .app_registry
            .test_age_entry("stub", REGISTRATION_TTL_MS + 1_000)
            .await
    );
    assert!(
        s.relay.app_registry.get("stub").await.is_some(),
        "the row is retained as a reservation"
    );
    let (status, body) = s.app_dispatch("stub").await.unwrap();
    assert_ne!(
        status, 200,
        "a reservation-only row was still routed to: {body}"
    );
}

/// The structural completion: an EXPLICIT release keeps its reservation on the
/// row, exactly like an expiry, so a well-behaved SDK is not punished for
/// behaving well.
///
/// The shape this replaces kept expiry on the row but put an explicit release
/// into a globally-bounded side map that unrelated principals could displace.
/// That inverted the incentive — an app that sent its `beforeunload` DELETE
/// moved its reservation from the unforgeable row into an attackable map and
/// ended up LESS protected than an app that simply vanished. Here the two
/// endings are pinned to behave identically.
#[tokio::test]
async fn an_explicit_release_reserves_the_id_exactly_like_an_expiry() {
    for release_explicitly in [true, false] {
        let s = spawn(BindingConfig::default()).await;
        let (status, _) = s
            .post(
                "/ui-bridge/apps/register",
                browser(GOOD),
                json!({ "appId": "app", "appName": "Victim", "appType": "web",
                        "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
            )
            .await;
        assert_eq!(status, 200);

        if release_explicitly {
            // The well-behaved path: `beforeunload` DELETE.
            let (status, body) = s
                .delete("/ui-bridge/apps/register/app", browser(GOOD))
                .await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(body["data"], true);
        } else {
            // The vanish path: stop heartbeating.
            assert!(
                s.relay
                    .app_registry
                    .test_age_entry("app", REGISTRATION_TTL_MS + 1_000)
                    .await
            );
        }

        // Either way it is gone from everything that routes or lists…
        assert!(
            s.relay
                .app_registry
                .list_live()
                .await
                .iter()
                .all(|e| e.app.app_id != "app"),
            "release_explicitly={release_explicitly}: still listed"
        );
        assert!(
            s.relay.app_registry.get_live("app").await.is_none(),
            "release_explicitly={release_explicitly}: still routable"
        );
        // …and either way the id is still RESERVED for its holder.
        let (status, body) = s
            .post(
                "/ui-bridge/apps/register",
                browser(EVIL),
                json!({ "appId": "app", "appName": "Squatter", "appType": "web",
                        "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
            )
            .await;
        assert_eq!(
            status, 409,
            "release_explicitly={release_explicitly}: the id was not reserved: {body}"
        );
        assert_eq!(code(&body), Some("UIB_REGISTRATION_HELD"), "{body}");

        // The holder itself gets it back at once.
        let (status, body) = s
            .post(
                "/ui-bridge/apps/register",
                browser(GOOD),
                json!({ "appId": "app", "appName": "Victim", "appType": "web",
                        "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
            )
            .await;
        assert_eq!(
            status, 200,
            "release_explicitly={release_explicitly}: holder locked out: {body}"
        );
    }
}

/// Critical 1, structurally: a reservation cannot be displaced by unrelated
/// principals, however many of them there are and whatever their origins sort
/// like.
///
/// The rule this replaces ranked candidates inside one globally-bounded map,
/// and every ranking tried was steerable. `share = MAX / buckets` is integer
/// division, so at ≥1024 buckets the share is 0 and "never evict at or under
/// your share" became vacuous in exactly the saturated regime the ceiling
/// existed for; and ties fell to the lexicographically greatest origin, which
/// an attacker simply picks (`http://` sorts below every `https://`).
///
/// The fixture below is the one that exposed it: the earlier test used
/// `s{i}.evil.example` and passed only because `'g' < 's'`. Here the attacker
/// origins sort BOTH ABOVE AND BELOW the victim's, and `http://` is included,
/// because the reservation no longer depends on any comparison at all.
#[tokio::test]
async fn a_reservation_is_not_displaceable_by_other_principals() {
    let s = spawn(BindingConfig::default()).await;
    let (status, _) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "victim-app", "appName": "Victim", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = s
        .delete("/ui-bridge/apps/register/victim-app", browser(GOOD))
        .await;
    assert_eq!(status, 200);

    // Sorting below the victim (`a…`, and `http://` below every `https://`),
    // above it (`z…`), and at the old fixture's value (`s…`).
    for (n, origin) in [
        "https://a0.evil.example",
        "https://a1.evil.example",
        "http://a2.evil.example",
        "https://s0.evil.example",
        "https://z0.evil.example",
        "http://localhost:3001",
    ]
    .iter()
    .enumerate()
    {
        let (status, body) = s
            .post(
                "/ui-bridge/apps/register",
                browser(origin),
                json!({ "appId": format!("squat-{n}"), "appName": "S", "appType": "web",
                        "transport": "http", "baseUrl": origin, "origin": origin }),
            )
            .await;
        assert_eq!(status, 200, "{origin}: {body}");
        let (status, _) = s
            .delete(
                &format!("/ui-bridge/apps/register/squat-{n}"),
                browser(origin),
            )
            .await;
        assert_eq!(status, 200);
    }

    // The victim's reservation is untouched by every one of them.
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "victim-app", "appName": "Squatter", "appType": "web",
                    "transport": "http", "baseUrl": EVIL, "origin": EVIL }),
        )
        .await;
    assert_eq!(
        status, 409,
        "the victim's reservation was displaced: {body}"
    );
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "victim-app", "appName": "Victim", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200, "the holder lost its own reservation: {body}");
}

/// Operator trust frees its id the moment it releases it — the same carve-out
/// the expiry arm has, on the release path.
#[tokio::test]
async fn an_operator_trust_release_frees_the_id_at_once() {
    let s = spawn(BindingConfig::default()).await;
    let (status, _) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": "app", "appName": "Synthetic", "appType": "web",
                    "transport": "http", "baseUrl": "http://127.0.0.1:65535" }),
        )
        .await;
    assert_eq!(status, 200);
    let (status, _) = s.delete("/ui-bridge/apps/register/app", agent()).await;
    assert_eq!(status, 200);
    assert!(
        s.relay.app_registry.get("app").await.is_none(),
        "an agent's released row must not linger as a reservation"
    );
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(GOOD),
            json!({ "appId": "app", "appName": "Page", "appType": "web",
                    "transport": "http", "baseUrl": GOOD, "origin": GOOD }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
}

/// Major 4: the ceilings bound BYTES as well as rows.
#[tokio::test]
async fn an_oversized_app_id_is_refused_at_the_register_door() {
    let s = spawn(BindingConfig::default()).await;
    let huge = "x".repeat(crate::mcp::app_registry::MAX_APP_ID_LEN + 1);
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": huge, "appName": "Big", "appType": "web",
                    "transport": "http", "baseUrl": "http://127.0.0.1:65535" }),
        )
        .await;
    assert_eq!(status, 400, "an unbounded appId was accepted: {body}");

    let ok = "x".repeat(crate::mcp::app_registry::MAX_APP_ID_LEN);
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": ok, "appName": "Big", "appType": "web",
                    "transport": "http", "baseUrl": "http://127.0.0.1:65535" }),
        )
        .await;
    assert_eq!(status, 200, "the cap is off by one: {body}");
}

/// R7.
#[tokio::test]
#[ignore = "red until Phase 1/2/3 — finding 8f142485"]
async fn sdk_switch_refused_to_foreign() {
    // R7 holds in EVERY route policy, so exercise the default
    // (`enforce-doors`, which only shadows this route today) and `off`, where
    // no allowlist is metered at all.
    for policy in [None, Some("off")] {
        let s = spawn_with_policy(policy, BindingConfig::default()).await;
        let (_w, ack) = s.ws_register(None, "app").await;
        assert_eq!(ack["type"], "registered");
        let (status, body) = s
            .post(
                "/ui-bridge/sdk/switch",
                browser(EVIL),
                json!({ "url": "ws-app://app" }),
            )
            .await;
        assert_eq!(
            status, 403,
            "a foreign origin reached sdk/switch under routePolicy={policy:?}: {body}"
        );
        assert_eq!(
            code(&body),
            Some(origin_guard::CODE_CROSS_ORIGIN_REFUSED),
            "{body}"
        );
    }
}

/// R1 is checked and written under one lock.
#[tokio::test]
async fn concurrent_claims_single_winner() {
    let s = Arc::new(spawn(BindingConfig::default()).await);
    let (_holder, ack) = s.ws_register(Some(GOOD), "app").await;
    let holder_conn = ack["connId"].as_u64().unwrap();

    let mut attempts = Vec::new();
    for _ in 0..50 {
        let s = s.clone();
        attempts.push(tokio::spawn(async move {
            let (ws, reply) = s.ws_register(Some(EVIL), "app").await;
            (ws, reply)
        }));
    }
    let mut held = 0;
    let mut sockets = Vec::new();
    for a in attempts {
        let (ws, reply) = a.await.unwrap();
        if ack_refusal_code(&reply) == Some("UIB_REGISTRATION_HELD") {
            held += 1;
        }
        sockets.push(ws);
        assert_eq!(
            conn_for(&s, "app").await,
            Some(holder_conn),
            "the routing slot moved"
        );
    }
    assert_eq!(held, 50, "every concurrent foreign claim must be refused");
}

/// R2's liveness arm.
#[tokio::test]
async fn displaced_conn_touch_does_not_refresh_holder() {
    let s = spawn(BindingConfig::default()).await;
    let (mut displaced, _) = s.ws_register(Some(GOOD), "app").await;
    let (_holder, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(s.relay.app_registry.test_age_entry("app", 20_000).await);
    let aged = s.relay.app_registry.get("app").await.unwrap().last_seen_ms;

    displaced
        .send(json!({ "type": "changeEvent", "event": {} }))
        .await;
    tokio::time::sleep(QUIET).await;
    let after = s.relay.app_registry.get("app").await.unwrap().last_seen_ms;
    assert_eq!(
        after, aged,
        "a displaced socket's frame refreshed the holder's entry"
    );
}

/// R4's transport arm.
#[tokio::test]
async fn browser_http_register_cannot_declare_websocket() {
    let s = spawn(BindingConfig::default()).await;
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "fresh", "appName": "x", "appType": "web",
                    "transport": "websocket", "websocketConnId": 1 }),
        )
        .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(code(&body), Some("UIB_ORIGIN_MISMATCH"), "{body}");
}

/// R3: no operator-trust exemption (the supervisor-laundered shape).
#[tokio::test]
#[ignore = "red until Phase 1/2/3 — finding 8f142485"]
async fn operator_trust_cannot_post_tab_result() {
    let s = spawn(BindingConfig::default()).await;
    let mut tab = s.attach("t1", browser(INJECT)).await.expect("attach");
    let dispatch = s.relay_dispatch(Some("t1"), 5_000);
    let cmd = tab.next(PROMPT).await.expect("command delivered");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();

    let (status, body) = s
        .post_result(agent(), "t1", &command_id, json!({ "laundered": true }))
        .await;
    assert_eq!(
        status, 403,
        "a no-Origin result post completed a tab's command: {body}"
    );
    assert_eq!(code(&body), Some("UIB_COMMAND_NOT_YOURS"), "{body}");
    tokio::time::sleep(QUIET).await;
    assert!(!dispatch.is_finished(), "the dispatch did not stay pending");

    s.post_result(
        browser(INJECT),
        "t1",
        &command_id,
        json!({ "genuine": true }),
    )
    .await;
    assert_eq!(dispatch.await.unwrap().1["data"]["result"]["genuine"], true);
}

/// Vector 6 (R8, enforce).
#[tokio::test]
#[ignore = "red until Phase 1/2/3 — finding 8f142485"]
async fn untargeted_dispatch_not_captured_during_reconnect() {
    let s = spawn(enforce_all()).await;
    let t1 = s.attach("t1", browser(INJECT)).await.expect("attach t1");
    t1.drop_stream();
    s.wait_tab_disconnected("t1").await;

    let mut t9 = s.attach("t9", browser(EVIL)).await.expect("attach t9");
    let (status, body) = s.relay_dispatch(None, 1_000).await.unwrap();
    assert!(
        t9.next(QUIET).await.is_none(),
        "the fresh foreign tab received the untargeted command"
    );
    assert_eq!(status, 409, "{body}");
    assert_eq!(code(&body), Some("AMBIGUOUS_TAB"), "{body}");
    // The candidates are a structured field, not prose: `suggestions` carries
    // one `retry with {"tabId": "…"}` entry per candidate. Each candidate gains
    // its `verifiedOrigin` in Phase 3 — assert that when it lands.
    let suggestions: Vec<String> = body["suggestions"]
        .as_array()
        .expect("AmbiguousTab lists its candidates in `suggestions`")
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    for tab in ["t1", "t9"] {
        assert!(
            suggestions
                .iter()
                .any(|s| s.contains(&format!("\"tabId\": \"{tab}\""))),
            "{tab} is not among the candidates: {suggestions:?}"
        );
    }
}

/// R9.
#[tokio::test]
#[ignore = "red until Phase 1/2/3 — finding 8f142485"]
async fn pinned_tab_key_binds_across_origins() {
    const K: &str = "k-0123456789abcdef0123456789abcdef";
    let s = spawn(BindingConfig::default()).await;
    let first = s.attach("t1", keyed(APP, K)).await.expect("keyed attach");
    first.drop_stream();
    s.wait_tab_disconnected("t1").await;

    let mut hop = s
        .attach("t1", keyed(ACCOUNTS, K))
        .await
        .expect("the keyed re-attach from the hop origin");
    let dispatch = s.relay_dispatch(Some("t1"), 5_000);
    let cmd = hop.next(PROMPT).await.expect("command delivered");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();
    let (status, _) = s
        .post_result(keyed(ACCOUNTS, K), "t1", &command_id, json!({ "ok": true }))
        .await;
    assert_eq!(status, 200);
    assert_eq!(dispatch.await.unwrap().0, 200);

    // The hop's stream is LIVE here, so the refusals below defend the keyed
    // holder rather than a stale record.
    assert_eq!(
        s.tab_entry("t1").await.expect("t1 listed")["connected"],
        true
    );
    for from in [browser(EVIL), keyed(EVIL, "k-some-other-key")] {
        match s.attach("t1", from).await {
            Err((status, body)) => assert_eq!(status, 409, "{body}"),
            Ok(_) => panic!("an attach without the tab's key took t1"),
        }
    }
}

/// The measured launch shape (Phase 0 step 1): the refusal and the keyed
/// exemption land in Phase 2; the real-origin re-attach is live in every phase.
mod pinned_tab_opaque_first_attach_then_real_origin {
    use super::*;

    fn opaque() -> Caller<'static> {
        browser("null")
    }

    /// Live in every phase: whatever happens to the `about:blank` attach, the
    /// same tab id attaching from the real page origin is admitted and served.
    #[tokio::test]
    async fn real_origin_reattach_admitted() {
        let s = spawn(BindingConfig::default()).await;
        if let Ok(blank) = s.attach("t1", opaque()).await {
            blank.drop_stream();
        }
        let mut real = s
            .attach("t1", browser(APP))
            .await
            .expect("the real-origin attach is admitted");
        let (status, _) = s
            .heartbeat("t1", browser(APP), "https://app.example/")
            .await;
        assert_eq!(status, 200);
        let dispatch = s.relay_dispatch(Some("t1"), 5_000);
        let cmd = real
            .next(PROMPT)
            .await
            .expect("command delivered to the real origin");
        let command_id = cmd["commandId"].as_str().unwrap().to_string();
        s.post_result(browser(APP), "t1", &command_id, json!({ "ok": true }))
            .await;
        assert_eq!(dispatch.await.unwrap().0, 200);
    }

    #[tokio::test]
    #[ignore = "red until Phase 1/2/3 — finding 8f142485"]
    async fn null_origin_refused() {
        let s = spawn(BindingConfig::default()).await;
        match s.attach("t1", opaque()).await {
            Err((status, body)) => {
                assert_eq!(status, 403, "{body}");
                assert_eq!(code(&body), Some("UIB_OPAQUE_ORIGIN"), "{body}");
            }
            Ok(_) => panic!("an opaque-origin stream attach was admitted"),
        }
        let (status, body) = s.heartbeat("t1", opaque(), "about:blank").await;
        assert_eq!(status, 403, "{body}");
        assert_eq!(code(&body), Some("UIB_OPAQUE_ORIGIN"), "{body}");
    }

    #[tokio::test]
    #[ignore = "red until Phase 1/2/3 — finding 8f142485"]
    async fn keyed_null_origin_admitted_and_bound_to_key() {
        const K: &str = "k-fedcba9876543210fedcba9876543210";
        let s = spawn(BindingConfig::default()).await;
        let blank = s
            .attach("t1", keyed("null", K))
            .await
            .expect("a keyed opaque attach is admitted");
        blank.drop_stream();
        s.wait_tab_disconnected("t1").await;
        let _real = s
            .attach("t1", keyed(APP, K))
            .await
            .expect("the same key re-attaches from the real origin");
        // Live holder, so the refusal below is the key binding and not a
        // tombstone or a stale record.
        assert_eq!(
            s.tab_entry("t1").await.expect("t1 listed")["connected"],
            true
        );
        match s.attach("t1", browser(APP)).await {
            Err((status, body)) => assert_eq!(status, 409, "{body}"),
            Ok(_) => panic!("t1 was bound to its origin, not to its key"),
        }
    }
}

// ===========================================================================
// Legitimate-flow tests — green on origin/main and after every phase
// ===========================================================================

#[tokio::test]
async fn agent_flow_unchanged() {
    let s = spawn(BindingConfig::default()).await;

    // A stub app the runner's HTTP dispatch arm can reach.
    let app = Router::new().route(
        "/dispatch",
        post(|Json(v): Json<Value>| async move { Json(json!({ "echo": v["action"] })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stub_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let stub = format!("http://127.0.0.1:{stub_port}");

    // Register a synthetic app with keepAliveSecs, and list it.
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": "synthetic", "appName": "Synthetic", "appType": "web",
                    "transport": "http", "baseUrl": stub, "keepAliveSecs": 3600 }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["data"]["appId"], "synthetic");
    assert_eq!(body["data"]["transport"], "http");
    let listed = s.registered("synthetic").await.expect("listed");
    assert_eq!(listed["transport"], "http");

    // Dispatch to it.
    let (status, body) = s.app_dispatch("synthetic").await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["echo"], "getControlSnapshot");

    // sdk/switch between two wrappers.
    let (_a, ack) = s.ws_register(None, "wrapper-a").await;
    assert_eq!(ack["type"], "registered");
    let (_b, ack) = s.ws_register(None, "wrapper-b").await;
    assert_eq!(ack["type"], "registered");
    assert_eq!(s.active_app_id().await.as_deref(), Some("wrapper-b"));
    let (status, body) = s
        .post(
            "/ui-bridge/sdk/switch",
            agent(),
            json!({ "url": "ws-app://wrapper-a" }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(s.active_app_id().await.as_deref(), Some("wrapper-a"));
    let (status, body) = s.get("/ui-bridge/sdk/connections", agent()).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"].as_array().map(Vec::len), Some(2), "{body}");

    // relay/dispatch to a tab.
    let mut tab = s.attach("tab-1", browser(INJECT)).await.expect("attach");
    let dispatch = s.relay_dispatch(Some("tab-1"), 5_000);
    let cmd = tab.next(PROMPT).await.expect("command delivered");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();
    s.post_result(browser(INJECT), "tab-1", &command_id, json!({ "n": 1 }))
        .await;
    let (status, body) = dispatch.await.unwrap();
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["data"]["tabId"], "tab-1");
    assert_eq!(body["data"]["result"]["n"], 1);

    // Operator trust displaces a browser holder.
    let (_holder, ack) = s.ws_register(Some(GOOD), "shared").await;
    assert_eq!(ack["type"], "registered");
    let (status, body) = s
        .post(
            "/ui-bridge/apps/register",
            agent(),
            json!({ "appId": "shared", "appName": "Shared", "appType": "web",
                    "transport": "http", "baseUrl": stub }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        s.relay.app_registry.get("shared").await.unwrap().transport,
        crate::mcp::app_registry::AppTransport::Http
    );

    // DELETE it.
    let (status, body) = s
        .delete("/ui-bridge/apps/register/synthetic", agent())
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["data"], true, "{body}");
    assert!(s.registered("synthetic").await.is_none());
}

#[tokio::test]
async fn node_wrapper_restart_reregisters() {
    let s = spawn(BindingConfig::default()).await;
    let (old, ack) = s.ws_register(None, "node-app").await;
    assert_eq!(ack["type"], "registered");
    // Killed: the socket goes away with no close frame.
    drop(old);

    let (_new, ack) = s.ws_register(None, "node-app").await;
    assert_eq!(ack["type"], "registered", "{ack}");
    let new_conn = ack["connId"].as_u64().unwrap();
    assert_eq!(conn_for(&s, "node-app").await, Some(new_conn));
    assert_eq!(s.active_app_id().await.as_deref(), Some("node-app"));
}

#[tokio::test]
async fn same_origin_last_tab_wins() {
    let s = spawn(BindingConfig::default()).await;
    let (mut first, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    let dispatch = s.app_dispatch("app");
    let _cmd = first.recv().await.expect("first tab receives the command");

    let (_second, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered", "{ack}");
    assert_eq!(conn_for(&s, "app").await, ack["connId"].as_u64());

    let (status, body) = dispatch.await.unwrap();
    assert_ne!(status, 200, "{body}");
    assert!(
        body.to_string().to_lowercase().contains("displaced"),
        "the first tab's in-flight command fails as Displaced: {body}"
    );
}

#[tokio::test]
async fn phone_home_refresh_and_beforeunload() {
    let s = spawn(BindingConfig::default()).await;
    for (origin, app_id, base_url) in [
        (WEB_DEV, "localhost", "http://localhost:3001/ui-bridge"),
        (
            "http://127.0.0.1:9875",
            "qontinui-supervisor-dashboard",
            "http://127.0.0.1:9875/supervisor-bridge",
        ),
    ] {
        let payload = json!({ "appId": app_id, "appName": "Dev", "appType": "web",
                              "transport": "http", "baseUrl": base_url,
                              "framework": "react", "capabilities": [], "origin": origin });
        for _ in 0..3 {
            let (status, body) = s
                .post("/ui-bridge/apps/register", browser(origin), payload.clone())
                .await;
            assert_eq!(status, 200, "{origin} re-POST: {body}");
        }
        assert!(s.registered(app_id).await.is_some());
        let (status, body) = s
            .delete(
                &format!("/ui-bridge/apps/register/{app_id}"),
                browser(origin),
            )
            .await;
        assert_eq!(status, 200, "{origin} keepalive DELETE: {body}");
        assert_eq!(body["data"], true);
    }
}

#[tokio::test]
async fn extension_content_script_shape() {
    let s = spawn(BindingConfig::default()).await;
    let (_ws, ack) = s
        .ws_register(Some("https://mail.example"), "mail.example-mail")
        .await;
    assert_eq!(ack["type"], "registered", "{ack}");
    assert_eq!(
        s.active_app_id().await.as_deref(),
        Some("mail.example-mail")
    );
}

#[tokio::test]
async fn injected_tab_full_cycle() {
    let s = spawn(BindingConfig::default()).await;
    let mut tab = s.attach("t1", browser(INJECT)).await.expect("attach");
    let (status, body) = s
        .heartbeat("t1", browser(INJECT), "https://inject.example/")
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["tabRegistered"], true, "{body}");

    for round in 0..2 {
        let dispatch = s.relay_dispatch(Some("t1"), 5_000);
        let cmd = tab.next(PROMPT).await.expect("command delivered");
        let command_id = cmd["commandId"].as_str().unwrap().to_string();
        let (status, body) = s
            .post_result(
                browser(INJECT),
                "t1",
                &command_id,
                json!({ "round": round }),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(body["data"]["matched"], true, "{body}");
        let (status, body) = dispatch.await.unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["data"]["result"]["round"], round);
        if round == 0 {
            tab.drop_stream();
            tab = s.attach("t1", browser(INJECT)).await.expect("re-attach");
        }
    }
}

#[tokio::test]
async fn phone_home_loopback_alias_same_principal() {
    let s = spawn(BindingConfig::default()).await;
    for origin in ["http://localhost:9875", "http://127.0.0.1:9875"] {
        let (status, body) = s
            .post(
                "/ui-bridge/apps/register",
                browser(origin),
                json!({ "appId": "qontinui-supervisor-dashboard", "appName": "Supervisor",
                        "appType": "web", "transport": "http",
                        "baseUrl": format!("{origin}/supervisor-bridge"), "origin": origin }),
            )
            .await;
        assert_eq!(status, 200, "{origin}: {body}");
    }
}

/// Admission is asserted in every phase; Phase 2 adds
/// `R9-unkeyed.wouldRefuse == 1`.
#[tokio::test]
async fn unkeyed_pinned_tab_origin_hop_shadow() {
    let s = spawn(BindingConfig::default()).await;
    let first = s.attach("t1", browser(APP)).await.expect("attach from app");
    // Wait for the runner to NOTICE the drop. Without this the re-attach races
    // a still-live listener, which is a plain cross-principal displacement
    // (R1, enforce by default) rather than the unkeyed re-attach this test is
    // the invariant for — so it would go red in Phase 2, or press Phase 2 into
    // weakening R1.
    first.drop_stream();
    s.wait_tab_disconnected("t1").await;
    let mut hop = s
        .attach("t1", browser(ACCOUNTS))
        .await
        .expect("the unkeyed re-attach from the hop origin is admitted");
    let dispatch = s.relay_dispatch(Some("t1"), 5_000);
    let cmd = hop
        .next(PROMPT)
        .await
        .expect("command delivered after the hop");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();
    s.post_result(browser(ACCOUNTS), "t1", &command_id, json!({ "ok": true }))
        .await;
    assert_eq!(dispatch.await.unwrap().0, 200);
}

/// Pins what the kill switches do: with both off, every attack succeeds again.
#[tokio::test]
async fn kill_switch_off_restores_today() {
    let s = spawn(BindingConfig {
        binding: BindingMode::Off,
        active_binding: BindingMode::Off,
    })
    .await;

    // WS hijack (vector 1) and active-connection steal (vector 2).
    let (_good, ack) = s.ws_register(Some(GOOD), "app").await;
    assert_eq!(ack["type"], "registered");
    let (_evil, ack) = s.ws_register(Some(EVIL), "app").await;
    assert_eq!(ack["type"], "registered", "{ack}");
    assert_eq!(conn_for(&s, "app").await, ack["connId"].as_u64());
    let (_other, _) = s.ws_register(Some(EVIL), "other").await;
    assert_eq!(s.active_app_id().await.as_deref(), Some("other"));

    // Tab stream takeover (vector 3) and forged completion (vector 4).
    let _victim = s.attach("t1", browser(INJECT)).await.expect("victim");
    let mut thief = s
        .attach("t1", browser(EVIL))
        .await
        .expect("takeover admitted");
    let dispatch = s.relay_dispatch(Some("t1"), 5_000);
    let cmd = thief
        .next(PROMPT)
        .await
        .expect("the thief receives the command");
    let command_id = cmd["commandId"].as_str().unwrap().to_string();
    let (status, body) = s
        .post_result(browser(EVIL), "t1", &command_id, json!({ "forged": true }))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["matched"], true);
    assert_eq!(dispatch.await.unwrap().1["data"]["result"]["forged"], true);

    // HTTP redirect over a WS holder (vector 5).
    let (status, _) = s
        .post(
            "/ui-bridge/apps/register",
            browser(EVIL),
            json!({ "appId": "app", "appName": "x", "appType": "web",
                    "transport": "http", "baseUrl": "https://evil.example" }),
        )
        .await;
    assert_eq!(status, 200);
    let (_, _) = s
        .delete("/ui-bridge/apps/register/other", browser(EVIL))
        .await;
    assert!(s.relay.app_registry.get("other").await.is_none());
}

#[test]
fn binding_config_from_values_parses() {
    // The two names `from_env` reads. Nothing else pins them, and a rename or
    // a transposition would silently break the documented kill switch while
    // every other test stayed green.
    assert_eq!(ENV_BINDING, "QONTINUI_RUNNER_UIBRIDGE_BINDING");
    assert_eq!(
        ENV_ACTIVE_BINDING,
        "QONTINUI_RUNNER_UIBRIDGE_ACTIVE_BINDING"
    );

    let d = BindingConfig::from_values(None, None);
    assert_eq!(d, BindingConfig::default());
    assert_eq!(d.binding, BindingMode::Enforce);
    assert_eq!(d.active_binding, BindingMode::Shadow);

    let off = BindingConfig::from_values(Some("off"), Some(" OFF "));
    assert_eq!(
        off,
        BindingConfig {
            binding: BindingMode::Off,
            active_binding: BindingMode::Off,
        }
    );
    let c = BindingConfig::from_values(Some("shadow"), Some("enforce"));
    assert_eq!(c.binding, BindingMode::Shadow);
    assert_eq!(c.active_binding, BindingMode::Enforce);
    let junk = BindingConfig::from_values(Some("yes"), Some(""));
    assert_eq!(junk, BindingConfig::default());
}

/// `Principal::same` is the ONE comparison every rule runs, and three of its
/// arms are not reachable from the end-to-end tests above: the `Opaque`
/// no-principal arm, the tab-key digest arm (Phase 2 consults it), and a
/// loopback alias on a DIFFERENT port or scheme, which must NOT fold.
#[test]
fn principal_same_folds_loopback_aliases_and_nothing_else() {
    let b = |o: &str| Principal::Browser {
        class: crate::mcp::origin_guard::OriginClass::Foreign,
        origin: NormOrigin::parse(o).expect("a parseable origin"),
    };
    let operator = Principal::OperatorTrust {
        class: crate::mcp::origin_guard::OriginClass::NonBrowser,
    };

    // The scheme's default port is made explicit on both sides.
    assert!(b("https://a.example").same(&b("https://a.example:443")));
    assert!(!b("https://a.example").same(&b("https://b.example")));
    assert!(!b("https://a.example").same(&b("http://a.example")));

    // Loopback aliases fold at the SAME scheme and port, and only there.
    for other in ["http://127.0.0.1:9875", "http://[::1]:9875"] {
        assert!(
            b("http://localhost:9875").same(&b(other)),
            "loopback alias {other} must be one principal"
        );
    }
    assert!(!b("http://localhost:9875").same(&b("http://localhost:3001")));
    assert!(!b("http://localhost:9875").same(&b("https://localhost:9875")));

    // Operator trust is one principal, and is not any browser.
    assert!(operator.same(&Principal::OperatorTrust {
        class: crate::mcp::origin_guard::OriginClass::FirstParty,
    }));
    assert!(!operator.same(&b("https://a.example")));
    // …but it may displace anything, which is the R1/R2 exemption.
    assert!(operator.may_displace(&b("https://a.example")));
    assert!(!b("https://a.example").may_displace(&operator));

    // A key binds by digest alone, whatever the origin.
    let k1 = Principal::TabKey {
        digest: key_digest("K"),
    };
    assert!(k1.same(&Principal::TabKey {
        digest: key_digest("K")
    }));
    assert!(!k1.same(&Principal::TabKey {
        digest: key_digest("other")
    }));
    assert!(!k1.same(&b("https://a.example")));

    // An opaque request has NO principal: it matches nothing, not even
    // another opaque one. Two attacker pages with no origin must not share a
    // claim.
    let opaque = Principal::Opaque {
        class: crate::mcp::origin_guard::OriginClass::Foreign,
    };
    assert!(!opaque.same(&opaque));
    assert!(!opaque.may_displace(&b("https://a.example")));
}

#[test]
fn counters_are_per_instance() {
    let a = RelayBinding::new(BindingConfig::default());
    let b = RelayBinding::new(BindingConfig::default());
    a.counters.record("R1", true);
    a.counters.record("R6", false);
    assert_eq!(a.counters.get("R1").refused, 1);
    assert_eq!(a.counters.get("R6").would_refuse, 1);
    assert_eq!(b.counters.get("R1"), RuleCount::default());
    assert_eq!(a.health_json()["rules"]["R1"]["refused"], 1);
}
