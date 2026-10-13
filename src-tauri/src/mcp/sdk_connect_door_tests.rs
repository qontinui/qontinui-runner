//! The SDK connect handshake at its door, with nothing injected (coord finding
//! `c89c2766`).
//!
//! [`connect_sdk_app`] is the handshake both production callers take
//! (`handle_connect` behind `POST /ui-bridge/sdk/connect`, and the
//! physical-device connect), and it finds the operator's stored credential by
//! itself. Each test here stores a device JWT the way a paired runner holds
//! one and calls it, so it observes the operator's bearer exactly where the
//! bearer would leave the runner.
//!
//! These tests live in a file of their own, with their own recorders, so that a
//! whole-file revert of `sdk_client.rs` (the "without the change" run of the
//! arming record) keeps them.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::mcp::sdk_client::{connect_sdk_app, SdkConnectionManager};

/// `(authority, Authorization)` of every request a recorder saw.
type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// Every variable reqwest's system proxy reads, in both spellings.
const PROXY_ENV: [&str; 8] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// What a UI Bridge app answers to its `/health` probe.
const HEALTH: &str = r#"{"uiBridge":{"appId":"door","appName":"Door","appType":"web"}}"#;

/// The ambient every door test runs in. Fields drop in order: the proxy
/// variables are restored while the env lock is still held, and the lock goes
/// last.
struct Door {
    _proxies: crate::test_env::EnvVarRestore,
    _ambient: crate::test_env::IsolatedAmbient,
}

/// Enter the door ambient:
/// - [`crate::test_env::isolated_ambient`] supplies the env lock and an empty
///   config and secure-storage dir;
/// - the keychain is switched off, so `store_tokens` never writes the
///   operator's real one;
/// - every proxy variable is cleared, so a proxy in the developer's shell is
///   never handed a request;
/// - an unexpired device JWT is stored where `connect_sdk_app` will find it.
fn door() -> Door {
    let ambient = crate::test_env::isolated_ambient();
    // Restored by the fixture: the key is in `ambient::AMBIENT_ENV_KEYS`.
    std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
    let proxies = crate::test_env::EnvVarRestore::capture(&PROXY_ENV);
    for key in PROXY_ENV {
        std::env::remove_var(key);
    }
    crate::auth::AuthManager::new()
        .store_tokens(&a_stored_device_jwt(), "")
        .expect("the fixture stores a device JWT");
    Door {
        _proxies: proxies,
        _ambient: ambient,
    }
}

/// An unexpired, JWT-shaped device token. It is unsigned: nothing on this path
/// verifies a signature, only the token's shape and its `exp`.
fn a_stored_device_jwt() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let exp = chrono::Utc::now().timestamp() + 3600;
    let payload =
        URL_SAFE_NO_PAD.encode(serde_json::json!({ "sub_type": "device", "exp": exp }).to_string());
    format!("{header}.{payload}.sig")
}

/// A UI Bridge app on 127.0.0.1. It answers every path like a healthy app and
/// records the `Host` and `Authorization` of each request.
async fn spawn_app() -> (u16, Seen) {
    let seen: Seen = Arc::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen_by_app = seen.clone();
    let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let seen = seen_by_app.clone();
        async move {
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned)
            };
            seen.lock()
                .unwrap()
                .push((header("host").unwrap_or_default(), header("authorization")));
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                HEALTH,
            )
        }
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, seen)
}

/// A forward proxy on 127.0.0.1, set through `HTTP_PROXY`. It records the
/// target authority and the `Authorization` of every request head it receives.
/// - A request for `stand_in` is answered by the proxy itself, as a healthy app.
/// - Any other loopback destination is spliced straight through, so a test
///   running alongside, whose client was built while `HTTP_PROXY` pointed
///   here, still reaches its own server.
/// - Anything else is refused with 502, so the proxy never dials off the
///   machine.
async fn spawn_proxy(stand_in: String) -> (u16, Seen) {
    let seen: Seen = Arc::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen_by_proxy = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let seen = seen_by_proxy.clone();
            let stand_in = stand_in.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if client.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let target = text
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|uri| url::Url::parse(uri).ok());
                let host = target
                    .as_ref()
                    .and_then(|t| t.host_str())
                    .unwrap_or_default()
                    .trim_matches(['[', ']'])
                    .to_string();
                let port = target
                    .as_ref()
                    .and_then(url::Url::port_or_known_default)
                    .unwrap_or(80);
                let authority = format!("{host}:{port}");
                let authorization = text.lines().find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
                        .map(|(_, value)| value.trim().to_string())
                });
                seen.lock()
                    .unwrap()
                    .push((authority.clone(), authorization));
                if authority == stand_in {
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{HEALTH}",
                        HEALTH.len()
                    );
                    let _ = client.write_all(reply.as_bytes()).await;
                    return;
                }
                let loopback = host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback());
                if loopback {
                    if let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await {
                        if upstream.write_all(&head).await.is_ok() {
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                        return;
                    }
                }
                let _ = client
                    .write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            });
        }
    });
    (port, seen)
}

/// The `Authorization` of every request a recorder saw for `authority`.
fn requests_to(seen: &Seen, authority: &str) -> Vec<Option<String>> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|(a, _)| a == authority)
        .map(|(_, auth)| auth.clone())
        .collect()
}

/// A foreign host never receives the operator's stored bearer.
///
/// `0.0.0.0` plays the foreign host. It is not a loopback literal and not a
/// Qontinui origin, yet on Linux and macOS a connection to it lands on the
/// local app, so the test makes no DNS lookup and contacts no real host.
/// Windows refuses a connect to `0.0.0.0`, hence `cfg(unix)`. The next test
/// checks the same property on every platform.
#[cfg(unix)]
#[tokio::test]
async fn the_handshake_never_hands_the_stored_bearer_to_a_foreign_host() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let url = format!("http://0.0.0.0:{port}");
    let _ = connect_sdk_app(&manager, &url, None, None, None, None, None).await;

    let probes = requests_to(&seen, &format!("0.0.0.0:{port}"));
    assert!(
        !probes.is_empty(),
        "the handshake never reached the stand-in foreign host"
    );
    assert!(
        probes.iter().all(Option::is_none),
        "a foreign host received the operator's stored bearer: {probes:?}"
    );
}

/// The same property on every platform, through a proxy. With `HTTP_PROXY`
/// set, a connect to a foreign host is carried by the proxy, and here the
/// proxy also plays that host. The host is `foreign.invalid`, a reserved name
/// that never resolves, so nothing leaves the machine. The proxy sees the
/// connect, and never the bearer.
#[tokio::test]
async fn a_foreign_host_behind_a_proxy_never_receives_the_stored_bearer() {
    let _door = door();
    let (proxy_port, seen) = spawn_proxy("foreign.invalid:80".to_string()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{proxy_port}"));
    let _ = connect_sdk_app(
        &manager,
        "http://foreign.invalid",
        None,
        None,
        None,
        None,
        None,
    )
    .await;
    std::env::remove_var("HTTP_PROXY");

    let probes = requests_to(&seen, "foreign.invalid:80");
    assert!(
        !probes.is_empty(),
        "the handshake never reached the stand-in foreign host through the proxy"
    );
    assert!(
        probes.iter().all(Option::is_none),
        "a foreign host received the operator's stored bearer: {probes:?}"
    );
}

/// A configured proxy never sees a loopback connect. The handshake dials a
/// loopback relay DIRECT, so the proxy receives neither the request nor the
/// operator's bearer, which would otherwise ride on it in forward-proxy form.
/// `NO_PROXY` is cleared, so the only thing that keeps the request off the
/// proxy is the connect's own client. The loopback relay is admitted, so it
/// does get the bearer, and gets it directly.
#[tokio::test]
async fn a_configured_proxy_never_sees_a_loopback_connect() {
    let _door = door();
    let (app_port, app_seen) = spawn_app().await;
    let (proxy_port, proxy_seen) = spawn_proxy(String::new()).await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let relay = format!("127.0.0.1:{app_port}");
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{proxy_port}"));
    let connected = connect_sdk_app(
        &manager,
        &format!("http://{relay}"),
        None,
        None,
        None,
        None,
        None,
    )
    .await;
    std::env::remove_var("HTTP_PROXY");

    let via_proxy = requests_to(&proxy_seen, &relay);
    assert!(
        via_proxy.is_empty(),
        "a loopback connect went through the proxy: {via_proxy:?}"
    );
    let direct = requests_to(&app_seen, &relay);
    assert!(
        connected.is_ok() && !direct.is_empty(),
        "the loopback relay was not reached directly: {direct:?}"
    );
    assert!(
        direct
            .iter()
            .all(|auth| auth.as_deref().is_some_and(|a| a.starts_with("Bearer "))),
        "an admitted loopback relay gets the bearer, directly: {direct:?}"
    );
}
