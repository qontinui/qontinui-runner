//! Acceptance tests for plan `2026-09-17-runner-loopback-api-accepts-any-origin`.
//!
//! The router under test carries the SAME two layers `create_router` applies
//! (`origin_guard::apply`) over probe handlers registered at the REAL route
//! patterns — `create_router` needs a live `tauri::AppHandle`, so the full
//! router cannot be built in a unit test. Every probe pattern is checked
//! against the runner's own route table (`probe_patterns_are_real_routes`), so
//! a probe cannot drift from the route it stands in for. Each probe handler
//! counts its calls: a refused request must leave the count at zero, which is
//! the "stub records zero calls" / "no `WindowMessage::Close` enqueued" /
//! "no file content" half of the plan's tests.

use super::*;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::Request as HttpRequest;
use axum::routing::{any, delete, get, post};
use std::collections::HashMap;
use std::sync::atomic::AtomicU16;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const EVIL: &str = "https://evil.example";
const WEB: &str = "http://localhost:3001";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Calls(Arc<Mutex<HashMap<&'static str, usize>>>);

impl Calls {
    fn hit(&self, k: &'static str) {
        *self.0.lock().unwrap().entry(k).or_default() += 1;
    }
    fn get(&self, k: &str) -> usize {
        self.0.lock().unwrap().get(k).copied().unwrap_or(0)
    }
}

struct Harness {
    guard: Arc<OriginGuard>,
    port: Arc<AtomicU16>,
    settings: Arc<Mutex<Vec<String>>>,
    calls: Calls,
    router: Router,
}

/// The probes: `(METHOD, real route pattern, call-counter key)`.
const PROBES: &[(&str, &str, &str)] = &[
    ("POST", "/ui-bridge/invoke/{command_name}", "token"),
    ("GET", "/files/read", "files_read"),
    ("GET", "/terminals/{id}/ws", "terminal_ws"),
    ("POST", "/ui-bridge/control/page/close-request", "close"),
    ("POST", "/ui-bridge/integration/read-file", "integration_read"),
    ("POST", "/ui-bridge/tauri/invoke", "tauri_invoke"),
    ("POST", "/graphql", "graphql"),
    ("GET", "/sessions", "sessions"),
    ("GET", "/unified-workflows", "workflows"),
    ("POST", "/scheduler/reconcile-now", "reconcile"),
    ("GET", "/health", "health"),
    ("GET", "/ui-bridge/ws", "ui_bridge_ws"),
    ("POST", "/ui-bridge/apps/register", "register"),
];

fn harness_with(policy: &str, guard_env: Option<&str>, origins_env: Option<&str>) -> Harness {
    let port = Arc::new(AtomicU16::new(crate::mcp::types::get_mcp_api_port()));
    let settings: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let p = port.clone();
    let s = settings.clone();
    let guard = Arc::new(OriginGuard::new(
        guard_env,
        Some(policy),
        origins_env,
        None,
        Arc::new(move || p.load(Ordering::Relaxed)),
        Arc::new(move || s.lock().unwrap().clone()),
    ));
    let calls = Calls::default();
    let mut router: Router = Router::new();
    for (method, pattern, key) in PROBES {
        let key: &'static str = key;
        let c = calls.clone();
        let g = guard.clone();
        router = match (*method, *pattern) {
            (_, "/terminals/{id}/ws") | (_, "/ui-bridge/ws") => {
                let h = move |ws: WebSocketUpgrade| {
                    let c = c.clone();
                    async move {
                        c.hit(key);
                        ws.on_upgrade(|_socket| async {})
                    }
                };
                if *pattern == "/ui-bridge/ws" {
                    router.route(pattern, any(h))
                } else {
                    router.route(pattern, get(h))
                }
            }
            ("GET", "/health") => router.route(
                pattern,
                get(move |r: Option<axum::Extension<RequesterClass>>| {
                    let c = c.clone();
                    let g = g.clone();
                    async move {
                        c.hit(key);
                        axum::Json(json!({ "originGuard": g.health_json(r.map(|e| e.0 .0)) }))
                    }
                }),
            ),
            ("GET", "/files/read") => router.route(
                pattern,
                get(move || {
                    let c = c.clone();
                    async move {
                        c.hit(key);
                        "SECRET-FILE-CONTENT"
                    }
                }),
            ),
            (m, _) => {
                let h = move || {
                    let c = c.clone();
                    async move {
                        c.hit(key);
                        axum::Json(json!({ "success": true, "data": "TOKEN-VALUE" }))
                    }
                };
                match m {
                    "GET" => router.route(pattern, get(h)),
                    "DELETE" => router.route(pattern, delete(h)),
                    _ => router.route(pattern, post(h)),
                }
            }
        };
    }
    let router = apply(router, guard.clone());
    Harness {
        guard,
        port,
        settings,
        calls,
        router,
    }
}

fn harness(policy: &str) -> Harness {
    harness_with(policy, None, None)
}

fn req(method: &str, uri: &str, headers: &[(&str, &str)], body: &str) -> HttpRequest<Body> {
    let mut b = HttpRequest::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn send(h: &Harness, r: HttpRequest<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = h.router.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

fn host(h: &Harness) -> String {
    format!("127.0.0.1:{}", h.port.load(Ordering::Relaxed))
}

fn acao(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn code(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["code"].as_str().map(str::to_string))
}

/// Serve the harness router on a real `127.0.0.1:0` listener and send a raw
/// WebSocket upgrade; returns the status line. `oneshot` cannot upgrade.
async fn ws_upgrade_status(h: &Harness, path: &str, origin: Option<&str>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    h.port.store(port, Ordering::Relaxed);
    let router = h.router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut raw = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n"
    );
    if let Some(o) = origin {
        raw.push_str(&format!("Origin: {o}\r\n"));
    }
    raw.push_str("\r\n");
    s.write_all(raw.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), s.read(&mut buf))
        .await
        .expect("upgrade response within 5s")
        .unwrap();
    String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Phase 1 acceptance tests
// ---------------------------------------------------------------------------

/// P1-1: a hostile preflight to the token door is refused and gets no CORS.
#[tokio::test]
async fn p1_01_foreign_preflight_to_token_door_refused_without_cors() {
    let h = harness("off");
    let (status, headers, _) = send(
        &h,
        req(
            "OPTIONS",
            "/ui-bridge/invoke/get_coord_device_token",
            &[
                ("host", &host(&h)),
                ("origin", EVIL),
                ("access-control-request-method", "POST"),
                ("access-control-request-headers", "content-type"),
            ],
            "",
        ),
    )
    .await;
    assert!(!status.is_success(), "preflight answered {status}");
    assert_eq!(acao(&headers), None);
}

/// P1-2: the real POST is refused typed, and the token stub never runs.
#[tokio::test]
async fn p1_02_foreign_post_to_token_door_refused_and_stub_not_called() {
    let h = harness("off");
    let (status, _, body) = send(
        &h,
        req(
            "POST",
            "/ui-bridge/invoke/get_coord_device_token",
            &[("host", &host(&h)), ("origin", EVIL), ("content-type", "application/json")],
            "{}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body).as_deref(), Some(CODE_CROSS_ORIGIN_REFUSED));
    assert!(!body.contains("TOKEN-VALUE"));
    assert_eq!(h.calls.get("token"), 0);
}

/// P1-3: DNS rebinding — no Origin, but a foreign Host — is refused.
#[tokio::test]
async fn p1_03_rebinding_host_refused_without_file_content() {
    let h = harness("off");
    let port = h.port.load(Ordering::Relaxed);
    let (status, _, body) = send(
        &h,
        req(
            "GET",
            "/files/read?path=/tmp/x.json",
            &[("host", &format!("evil.example:{port}"))],
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body).as_deref(), Some(CODE_HOST_NOT_LOOPBACK));
    assert!(!body.contains("SECRET-FILE-CONTENT"));
    assert_eq!(h.calls.get("files_read"), 0);
}

/// P1-4: a terminal WebSocket upgrade from a foreign origin is 403, not 101.
#[tokio::test]
async fn p1_04_foreign_terminal_ws_upgrade_refused_before_upgrade() {
    let h = harness("off");
    let line = ws_upgrade_status(&h, "/terminals/abc/ws", Some(EVIL)).await;
    assert!(line.contains(" 403"), "status line: {line}");
    assert_eq!(h.calls.get("terminal_ws"), 0);
}

/// P1-5: a no-preflight text/plain POST to close-request is refused.
#[tokio::test]
async fn p1_05_simple_request_close_refused_nothing_enqueued() {
    let h = harness("off");
    let (status, _, _) = send(
        &h,
        req(
            "POST",
            "/ui-bridge/control/page/close-request",
            &[("host", &host(&h)), ("origin", EVIL), ("content-type", "text/plain")],
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(h.calls.get("close"), 0);
}

/// P1-6: the autonomy regression guard — no Origin, loopback Host → as today.
#[tokio::test]
async fn p1_06_agent_request_without_origin_unchanged() {
    for policy in ["enforce", "enforce-foreign", "shadow", "off"] {
        let h = harness(policy);
        let (status, _, body) = send(
            &h,
            req(
                "POST",
                "/ui-bridge/invoke/get_coord_device_token",
                &[("host", &host(&h)), ("content-type", "application/json")],
                "{}",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "policy {policy}: {body}");
        assert_eq!(h.calls.get("token"), 1);
    }
}

/// P1-7: the webview regression guard.
#[tokio::test]
async fn p1_07_webview_origins_admitted() {
    for origin in ["http://tauri.localhost", "tauri://localhost", "https://tauri.localhost"] {
        let h = harness("enforce");
        let (status, headers, _) = send(
            &h,
            req(
                "POST",
                "/ui-bridge/invoke/get_coord_device_token",
                &[("host", &host(&h)), ("origin", origin), ("content-type", "application/json")],
                "{}",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{origin}");
        assert_eq!(acao(&headers).as_deref(), Some(origin));
        assert_eq!(h.calls.get("token"), 1);
    }
}

/// P1-8: `QONTINUI_RUNNER_ORIGIN_GUARD=0` is the bypass, and it works.
#[tokio::test]
async fn p1_08_kill_switch_disables_the_guard() {
    let h = harness_with("enforce", Some("0"), None);
    assert!(!h.guard.enabled());
    let (status, headers, _) = send(
        &h,
        req(
            "POST",
            "/ui-bridge/invoke/get_coord_device_token",
            &[("host", "evil.example:1"), ("origin", EVIL), ("content-type", "application/json")],
            "{}",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Even disabled, CORS echoes the exact origin — never `*`.
    assert_eq!(acao(&headers).as_deref(), Some(EVIL));
    // Any other value leaves it on.
    for v in [None, Some("1"), Some("false"), Some("")] {
        assert!(harness_with("off", v, None).guard.enabled(), "{v:?}");
    }
}

/// P1-9: every loopback spelling on the BOUND port, and only the bound port.
#[tokio::test]
async fn p1_09_host_gate_uses_the_bound_port() {
    let h = harness("off");
    let desired = crate::mcp::types::get_mcp_api_port();
    let bound = desired.wrapping_add(1);
    h.port.store(bound, Ordering::Relaxed);
    for host_value in [
        format!("localhost:{bound}"),
        format!("LOCALHOST:{bound}"),
        format!("[::1]:{bound}"),
        format!("127.0.0.1:{bound}"),
    ] {
        let (status, _, _) = send(&h, req("GET", "/sessions", &[("host", &host_value)], "")).await;
        assert_eq!(status, StatusCode::OK, "{host_value}");
    }
    let (status, _, body) = send(
        &h,
        req("GET", "/sessions", &[("host", &format!("127.0.0.1:{desired}"))], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "desired-but-unbound port admitted");
    assert_eq!(code(&body).as_deref(), Some(CODE_HOST_NOT_LOOPBACK));
    // Missing Host (HTTP/1.0) is admitted.
    let (status, _, _) = send(&h, req("GET", "/sessions", &[], "")).await;
    assert_eq!(status, StatusCode::OK);
}

/// P1-10: the doors the draft missed.
#[tokio::test]
async fn p1_10_integration_read_file_tauri_invoke_and_graphql_refused() {
    let h = harness("off");
    for (path, ctype, body, key) in [
        ("/ui-bridge/integration/read-file", "application/json", r#"{"project_path":"/","file_path":"x"}"#, "integration_read"),
        ("/ui-bridge/tauri/invoke", "application/json", r#"{"command":"terminal_create"}"#, "tauri_invoke"),
        ("/graphql", "text/plain", r#"{"query":"mutation { uiBridgeEvaluate(code: \"1\") }"}"#, "graphql"),
    ] {
        let (status, _, resp) = send(
            &h,
            req("POST", path, &[("host", &host(&h)), ("origin", EVIL), ("content-type", ctype)], body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(code(&resp).as_deref(), Some(CODE_CROSS_ORIGIN_REFUSED), "{path}");
        assert_eq!(h.calls.get(key), 0, "{path}");
    }
}

/// P1-11: a cross-site no-cors GET carries no Origin but does carry Fetch
/// Metadata; it is Foreign. Without that header it is an agent.
#[tokio::test]
async fn p1_11_cross_site_fetch_metadata_without_origin_is_foreign() {
    let h = harness("off");
    let (status, _, body) = send(
        &h,
        req(
            "GET",
            "/files/read?path=/tmp/x.json",
            &[("host", &host(&h)), ("sec-fetch-site", "cross-site")],
            "",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body).as_deref(), Some(CODE_CROSS_ORIGIN_REFUSED));
    assert!(!body.contains("SECRET-FILE-CONTENT"));
    let (status, _, body) = send(
        &h,
        req("GET", "/files/read?path=/tmp/x.json", &[("host", &host(&h))], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "SECRET-FILE-CONTENT");
    // A top-level navigation (`none`) and a same-origin page are not browsers-as-attackers.
    for sfs in ["none", "same-origin"] {
        let (status, _, _) = send(
            &h,
            req("GET", "/files/read", &[("host", &host(&h)), ("sec-fetch-site", sfs)], ""),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{sfs}");
    }
}

// P1-12 lives in `mcp::backend_relay::tests::relay_strips_browser_origin_and_fetch_metadata`.
// P1-13 lives in `mcp::dom_capture::tests::captured_html_is_served_sandboxed`.

/// P1-14: a first-party preflight is classified by the method it asks about.
#[tokio::test]
async fn p1_14_webview_preflight_admitted_with_exact_origin() {
    let h = harness("enforce");
    let (status, headers, _) = send(
        &h,
        req(
            "OPTIONS",
            "/ui-bridge/invoke/get_coord_device_token",
            &[
                ("host", &host(&h)),
                ("origin", "http://tauri.localhost"),
                ("access-control-request-method", "POST"),
            ],
            "",
        ),
    )
    .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(acao(&headers).as_deref(), Some("http://tauri.localhost"));
    assert_eq!(h.calls.get("token"), 0, "a preflight never reaches the handler");
}

// ---------------------------------------------------------------------------
// Phase 2 acceptance tests
// ---------------------------------------------------------------------------

/// P2-1: a foreign origin on an ordinary route is refused, with no CORS.
#[tokio::test]
async fn p2_01_foreign_ordinary_route_refused() {
    let h = harness("enforce");
    let (status, headers, body) = send(
        &h,
        req("GET", "/sessions", &[("host", &host(&h)), ("origin", EVIL)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(acao(&headers), None);
    assert_eq!(h.calls.get("sessions"), 0);
}

/// P2-2: the web dev frontend on an allowlisted route: exact echo + Vary.
#[tokio::test]
async fn p2_02_trusted_allowlisted_route_echoes_exact_origin() {
    let h = harness("enforce");
    let (status, headers, _) = send(
        &h,
        req("GET", "/unified-workflows", &[("host", &host(&h)), ("origin", WEB)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acao(&headers).as_deref(), Some(WEB));
    let vary = headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(",")
        .to_ascii_lowercase();
    assert!(vary.contains("origin"), "vary: {vary}");
}

/// P2-3 + P2-9 (trusted half): a mutating route with no web caller is refused
/// to the trusted class, and that 403 is readable by the page.
#[tokio::test]
async fn p2_03_trusted_off_allowlist_route_refused_readably() {
    assert!(!TRUSTED_ROUTES.contains(&("POST", "/scheduler/reconcile-now")));
    let h = harness("enforce");
    let (status, headers, body) = send(
        &h,
        req("POST", "/scheduler/reconcile-now", &[("host", &host(&h)), ("origin", WEB)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["code"], CODE_CROSS_ORIGIN_REFUSED);
    assert_eq!(v["context"]["class"], "trusted");
    assert_eq!(v["context"]["route_pattern"], "/scheduler/reconcile-now");
    assert_eq!(v["context"]["admitOriginSetting"], SETTINGS_FIELD);
    assert_eq!(acao(&headers).as_deref(), Some(WEB));
    assert_eq!(h.calls.get("reconcile"), 0);
}

/// P2-4: the extension content-script shape (any page origin) keeps working.
#[tokio::test]
async fn p2_04_extension_page_origin_reaches_health_and_ui_bridge_ws() {
    let h = harness("enforce");
    let origin = "https://github.com";
    let (status, headers, _) = send(
        &h,
        req("GET", "/health", &[("host", &host(&h)), ("origin", origin)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acao(&headers).as_deref(), Some(origin));
    let line = ws_upgrade_status(&h, "/ui-bridge/ws", Some(origin)).await;
    assert!(line.contains(" 101"), "status line: {line}");
}

/// P2-5: an env-configured origin is admitted on the trusted allowlist only.
#[tokio::test]
async fn p2_05_env_allowed_origin_is_trusted() {
    let h = harness_with("enforce", None, Some("http://localhost:5173, http://example.test:8080"));
    let o = "http://localhost:5173";
    let (status, headers, _) = send(
        &h,
        req("GET", "/unified-workflows", &[("host", &host(&h)), ("origin", o)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acao(&headers).as_deref(), Some(o));
    let (status, _, body) = send(
        &h,
        req("POST", "/ui-bridge/invoke/get_coord_device_token", &[("host", &host(&h)), ("origin", o)], "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "trusted never reaches a door");
    assert_eq!(code(&body).as_deref(), Some(CODE_CROSS_ORIGIN_REFUSED));
}

/// P2-6 tripwire: no credential door is on a browser allowlist.
/// Mutation proof: add `("POST", "/ui-bridge/invoke/{command_name}")` to
/// `TRUSTED_ROUTES` and this fails.
#[test]
fn p2_06_credential_doors_disjoint_from_allowlists() {
    let mut bad = Vec::new();
    for (m, p) in TRUSTED_ROUTES.iter().chain(FOREIGN_ROUTES.iter()) {
        if is_credential_door(m, p) {
            bad.push(format!("{m} {p}"));
        }
    }
    assert!(bad.is_empty(), "credential doors on a browser allowlist: {bad:?}");
}

/// P2-7 tripwire: every allowlist entry names a registered route EXACTLY
/// (the guard compares for equality with `MatchedPath`, placeholder names
/// included). Mutation proof: rename one entry and this fails.
#[test]
fn p2_07_every_allowlist_entry_is_a_registered_route() {
    let registered = crate::mcp::relay_path_policy::tests::registered_routes();
    assert!(registered.len() > 500, "route scan broken: {}", registered.len());
    let mut missing = Vec::new();
    for (m, p) in TRUSTED_ROUTES.iter().chain(FOREIGN_ROUTES.iter()) {
        let hit = registered
            .iter()
            .any(|(rm, rp)| rp == p && (rm == m || rm == "ANY"));
        if !hit {
            missing.push(format!("{m} {p}"));
        }
    }
    assert!(missing.is_empty(), "allowlisted but not registered: {missing:?}");
}

/// P2-8: shadow admits, and says so.
#[tokio::test]
async fn p2_08_shadow_admits_and_counts() {
    let h = harness("shadow");
    let (status, _, _) = send(
        &h,
        req("GET", "/sessions", &[("host", &host(&h)), ("origin", EVIL)], ""),
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_eq!(h.calls.get("sessions"), 1);
    let v = h.guard.health_json(None);
    assert_eq!(v["shadowWouldRefuse"]["foreign"], 1);
    assert_eq!(v["recent"][0]["verdict"], "shadow_would_refuse");
    // Shadow never lifts the Phase 1 door refusal.
    let (status, _, _) = send(
        &h,
        req("POST", "/ui-bridge/invoke/x", &[("host", &host(&h)), ("origin", EVIL)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// P2-9: a Foreign refusal carries no CORS; `/health` withholds the recent
/// tuples from a Foreign requester and shows them to the webview.
#[tokio::test]
async fn p2_09_foreign_refusal_opaque_and_health_tuples_withheld() {
    let h = harness("enforce");
    let (_, headers, _) = send(
        &h,
        req("GET", "/sessions", &[("host", &host(&h)), ("origin", EVIL)], ""),
    )
    .await;
    assert_eq!(acao(&headers), None);
    let (status, _, body) = send(
        &h,
        req("GET", "/health", &[("host", &host(&h)), ("origin", "https://github.com")], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["originGuard"].get("recent").is_none(), "{v}");
    assert!(!body.contains("evil.example"));
    let (_, _, body) = send(
        &h,
        req("GET", "/health", &[("host", &host(&h)), ("origin", "tauri://localhost")], ""),
    )
    .await;
    assert!(body.contains("evil.example"), "first party sees the tuples: {body}");
}

/// The shipped default: Foreign enforced, Trusted shadowed.
#[tokio::test]
async fn default_policy_enforces_foreign_and_shadows_trusted() {
    assert_eq!(RoutePolicy::parse(None), RoutePolicy::EnforceForeign);
    assert_eq!(RoutePolicy::parse(Some("bogus")), DEFAULT_ROUTE_POLICY);
    assert_eq!(RoutePolicy::parse(Some(" ENFORCE ")), RoutePolicy::Enforce);
    let h = harness("");
    assert_eq!(h.guard.route_policy(), RoutePolicy::EnforceForeign);
    let (status, _, _) = send(
        &h,
        req("GET", "/sessions", &[("host", &host(&h)), ("origin", EVIL)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign enforced by default");
    let (status, _, _) = send(
        &h,
        req("POST", "/scheduler/reconcile-now", &[("host", &host(&h)), ("origin", WEB)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trusted shadowed by default");
    assert_eq!(h.guard.health_json(None)["shadowWouldRefuse"]["trusted"], 1);
}

// ---------------------------------------------------------------------------
// Phase 3 (runner half): settings-driven origins, live without a rebuild
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p3_settings_origin_admitted_on_next_request_without_rebuild() {
    let h = harness("enforce");
    let o = "http://localhost:4200";
    let (status, _, _) = send(
        &h,
        req("GET", "/unified-workflows", &[("host", &host(&h)), ("origin", o)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "not yet configured");
    h.settings.lock().unwrap().push(o.to_string());
    let (status, headers, _) = send(
        &h,
        req("GET", "/unified-workflows", &[("host", &host(&h)), ("origin", o)], ""),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "same router, next request");
    assert_eq!(acao(&headers).as_deref(), Some(o));
}

#[test]
fn p3_settings_field_round_trips() {
    let s: crate::settings::Settings =
        serde_json::from_str(r#"{"api":{"allowed_origins":["http://localhost:4200"]}}"#).unwrap();
    assert_eq!(s.api.allowed_origins, vec!["http://localhost:4200".to_string()]);
    let d = crate::settings::Settings::default();
    assert!(d.api.allowed_origins.is_empty());
}

// ---------------------------------------------------------------------------
// Harness honesty, route census, pure helpers
// ---------------------------------------------------------------------------

/// Every probe stands in for a route the runner really registers.
#[test]
fn probe_patterns_are_real_routes() {
    let registered = crate::mcp::relay_path_policy::tests::registered_routes();
    for (m, p, _) in PROBES {
        assert!(
            registered.iter().any(|(rm, rp)| rp == p && (rm == m || rm == "ANY")),
            "probe {m} {p} is not a registered route"
        );
    }
}

/// Every door entry covers at least one registered route — a door that
/// names nothing protects nothing. `/graphql/ws` is a `route_service`, which
/// the literal `.route(` scan cannot see.
#[test]
fn every_credential_door_covers_a_registered_route() {
    let registered = crate::mcp::relay_path_policy::tests::registered_routes();
    let mut dead = Vec::new();
    for entry in CREDENTIAL_DOORS {
        if *entry == "/graphql/ws" {
            continue;
        }
        let hit = registered.iter().any(|(m, p)| {
            if m == "ANY" {
                ["GET", "POST", "PUT", "PATCH", "DELETE"]
                    .iter()
                    .any(|mm| door_matches(entry, mm, p))
            } else {
                door_matches(entry, m, p)
            }
        });
        if !hit {
            dead.push(*entry);
        }
    }
    assert!(dead.is_empty(), "doors covering no registered route: {dead:?}");
}

/// Routes whose path is SHAPED like a credential, file or execution door —
/// see [`SENSITIVE_SHAPE`] — that the Phase 0 census (handler bodies read at
/// qontinui-runner a85ec319b) judged NOT to be doors, or that live only in
/// test modules. Anything shaped like a door and on no list fails
/// `every_door_shaped_route_is_classified`.
const REVIEWED_NOT_DOOR: &[(&str, &str)] = &[
    ("GET", "/agent-tokens/health"),
    ("GET", "/agent-worktrees/reclaimable"),
    ("GET", "/agent-worktrees/wip-orphans"),
    ("GET", "/analytics/token-usage/by-model"),
    ("GET", "/analytics/token-usage/by-page"),
    ("GET", "/analytics/token-usage/by-phase"),
    ("GET", "/analytics/token-usage/by-provider"),
    ("GET", "/analytics/token-usage/by-target-app"),
    ("GET", "/analytics/token-usage/cost-per-interaction"),
    ("GET", "/analytics/token-usage/daily"),
    ("GET", "/analytics/token-usage/model-action-matrix"),
    ("GET", "/analytics/token-usage/page-complexity"),
    ("GET", "/analytics/token-usage/summary"),
    ("GET", "/analytics/token-usage/task-runs"),
    ("POST", "/api/v1/devices/pair-cli"),
    ("POST", "/api/v1/devices/pair-codes/{code}/redeem"),
    ("POST", "/api/v1/devices/{device_id}/machine-credential/exchange"),
    ("GET", "/auth/runner-token-callback"),
    ("POST", "/backup/info"),
    ("POST", "/checks/repair-associations"),
    ("GET", "/control/sessions/restore-census"),
    ("GET", "/control/sessions/restore-health"),
    ("GET", "/current-execution/steps"),
    ("POST", "/devices/{device_id}/refresh-token"),
    ("GET", "/evaluation/cache-stats"),
    ("POST", "/evaluation/repair-guidance"),
    ("POST", "/evaluation/step"),
    ("POST", "/evaluation/workflow"),
    ("GET", "/execution-spans"),
    ("GET", "/file-activity/heatmap"),
    ("GET", "/file-activity/heatmap-live"),
    ("GET", "/file-locks/info"),
    ("POST", "/file-locks/signal-long-wait"),
    ("POST", "/file-locks/yield"),
    ("POST", "/file-locks/yield-request"),
    ("POST", "/file-registry/check-conflicts"),
    ("GET", "/file-registry/info"),
    ("POST", "/file-registry/probe-conflicts"),
    ("POST", "/file-registry/register"),
    ("POST", "/file-registry/release-all"),
    ("POST", "/file-registry/unregister"),
    ("PUT", "/hooks/reorder"),
    ("DELETE", "/image-quality-tests/image/{category}/{filename}"),
    ("GET", "/image-quality-tests/image/{category}/{filename}"),
    ("PUT", "/image-quality-tests/image/{category}/{filename}"),
    ("PUT", "/log-sources/default-profile"),
    ("GET", "/mcp/memory/tool-descriptor"),
    ("POST", "/memory/dreamer/run"),
    ("GET", "/memory/entity-profiles"),
    ("POST", "/memory/entity-profiles/generate"),
    ("POST", "/memory/entity-profiles/refresh"),
    ("GET", "/memory/entity-profiles/search"),
    ("GET", "/memory/entity-profiles/{kind}/{id}"),
    ("GET", "/meta-optimizer/canaries/{id}/evaluation"),
    ("POST", "/meta-optimizer/evaluate-with-io"),
    ("POST", "/meta-optimizer/recommendations/{id}/evaluate"),
    ("POST", "/reflection/evaluate"),
    ("GET", "/restart-readiness"),
    ("GET", "/restate/workflows/{execution_id}/state"),
    ("GET", "/sessions/{id}/touched-files"),
    ("GET", "/settings/playwright/has-password"),
    ("GET", "/spawn-placement/preview"),
    ("GET", "/spawn-placement/temp"),
    ("GET", "/spawn-placement/temps"),
    ("PUT", "/spawn-placement/temps"),
    ("GET", "/state-machine/blocked-triggers"),
    ("GET", "/state-machine/permitted-triggers"),
    ("POST", "/stop-execution"),
    ("GET", "/terminal-pages"),
    ("POST", "/tests/execute-suite"),
    ("GET", "/ui-bridge/control/design/evaluate/contexts"),
    ("GET", "/ui-bridge/control/transition/{id}/can-execute"),
    ("POST", "/ui-bridge/design/evaluate"),
    ("POST", "/ui-bridge/design/evaluate/baseline"),
    ("GET", "/ui-bridge/design/evaluate/contexts"),
    ("POST", "/ui-bridge/design/evaluate/diff"),
    ("GET", "/ui-bridge/diagnostics/readiness"),
    ("GET", "/ui-bridge/sdk/control/transition/{id}/can-execute"),
    ("POST", "/ui-bridge/sdk/design/evaluate"),
    ("POST", "/ui-bridge/sdk/design/evaluate/baseline"),
    ("GET", "/ui-bridge/sdk/design/evaluate/contexts"),
    ("POST", "/ui-bridge/sdk/design/evaluate/diff"),
    ("GET", "/ui-bridge/sdk/transition/{id}/can-execute"),
];

/// Path fragments that make a route a door suspect.
const SENSITIVE_SHAPE: &[&str] = &[
    "token", "credential", "secret", "password", "api-key", "exec", "spawn", "/run", "shell",
    "terminal", "file", "invoke", "evaluat", "restart", "drain", "install", "backup", "restore",
    "transcript", "clipboard", "pair", "launch", "script", "python", "process", "worktree",
    "webhook", "hook", "trigger", "tunnel", "jwt", "nonce", "/write", "/read",
];

/// The census tripwire: a route added tomorrow whose path looks like a door
/// must be classified — made a door, allowlisted, or reviewed — before CI goes
/// green. Under `enforce` an unclassified route is already refused to
/// browsers; this keeps `shadow`/`off` and the Trusted class honest too.
#[test]
fn every_door_shaped_route_is_classified() {
    let registered = crate::mcp::relay_path_policy::tests::registered_routes();
    let mut unclassified = Vec::new();
    for (m, p) in &registered {
        let lower = p.to_ascii_lowercase();
        let shaped = SENSITIVE_SHAPE.iter().any(|k| {
            if *k == "/run" {
                lower.ends_with("/run") || lower.contains("/run/")
            } else {
                lower.contains(k)
            }
        });
        if !shaped {
            continue;
        }
        let methods: Vec<&str> = if m == "ANY" {
            vec!["GET", "POST", "PUT", "PATCH", "DELETE"]
        } else {
            vec![m.as_str()]
        };
        let door = methods.iter().any(|mm| is_credential_door(mm, p));
        let listed = |list: &[(&str, &str)]| list.iter().any(|(lm, lp)| lm == m && lp == p);
        if door || listed(TRUSTED_ROUTES) || listed(FOREIGN_ROUTES) || listed(REVIEWED_NOT_DOOR) {
            continue;
        }
        unclassified.push(format!("{m} {p}"));
    }
    unclassified.sort();
    assert!(
        unclassified.is_empty(),
        "door-shaped routes on no list — add each to CREDENTIAL_DOORS, an allowlist, \
         or REVIEWED_NOT_DOOR after reading its handler: {unclassified:?}"
    );
}

#[test]
fn canonical_allowed_origin_validates() {
    assert_eq!(canonical_allowed_origin(" http://LocalHost:5173/ ").unwrap(), "http://localhost:5173");
    assert_eq!(canonical_allowed_origin("https://example.test:443").unwrap(), "https://example.test");
    assert!(canonical_allowed_origin("*").is_err());
    assert!(canonical_allowed_origin("null").is_err());
    assert!(canonical_allowed_origin("http://localhost:5173/app").is_err());
    assert!(canonical_allowed_origin("localhost:5173").is_err());
    assert!(canonical_allowed_origin("tauri://localhost").is_err());
}

/// A rathole-tunnelled request keeps the tunnel server's Host; it is admitted
/// only while that tunnel is up.
#[tokio::test]
async fn tunnel_host_admitted_only_while_tunnel_up() {
    let h = harness("off");
    let r = || req("GET", "/sessions", &[("host", "relay.example.test:5202")], "");
    set_tunnel_server_addr(Some("relay.example.test:2333"));
    let (status, _, _) = send(&h, r()).await;
    set_tunnel_server_addr(None);
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&h, r()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Review finding: a server-side fetch of a caller URL launders a browser
/// request into a NonBrowser one, so it is a door even for Trusted.
#[test]
fn loopback_laundering_routes_are_doors() {
    assert!(is_credential_door("POST", "/api-request/test"));
    assert!(is_credential_door("POST", "/ui-bridge/ai/network-probe"));
}

#[test]
fn door_grammar() {
    assert!(door_matches("/files/*", "GET", "/files/read"));
    assert!(!door_matches("/files/*", "GET", "/files"));
    assert!(door_matches("/terminals", "POST", "/terminals"));
    assert!(door_matches("/wrappers/{id}/credentials/*", "PUT", "/wrappers/{wid}/credentials/{name}"));
    assert!(door_matches("POST /ui-bridge/control/*", "POST", "/ui-bridge/control/fill"));
    assert!(!door_matches("POST /ui-bridge/control/*", "GET", "/ui-bridge/control/elements"));
    assert!(!door_matches("/sessions/spawn", "POST", "/sessions/spawned"));
}

#[test]
fn origin_normalisation() {
    let g = harness("off").guard;
    let p = (g.bound_port)();
    let fp = |o: &str| NormOrigin::parse(o).map(|n| g.is_first_party(&n)).unwrap_or(false);
    assert!(fp("tauri://localhost"));
    assert!(fp("http://tauri.localhost"));
    assert!(fp("HTTP://Tauri.Localhost/"));
    assert!(fp(&format!("http://127.0.0.1:{p}")));
    assert!(fp(&format!("http://[::1]:{p}")));
    assert!(!fp(&format!("http://127.0.0.1:{}", p.wrapping_add(7))));
    assert!(!fp("http://tauri.localhost.evil.example"));
    assert!(!fp("null"));
    assert!(NormOrigin::parse("null").is_none());
}
