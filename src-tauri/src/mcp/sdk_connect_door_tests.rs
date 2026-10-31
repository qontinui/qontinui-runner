//! The SDK connect handshake at its door, with nothing injected (coord finding
//! `c89c2766`).
//!
//! [`connect_sdk_app`] is the handshake the physical-device connect takes, and
//! [`connect_or_reuse`] the body of `handle_connect` behind
//! `POST /ui-bridge/sdk/connect`. Both find the operator's stored credential by
//! themselves. Each test here stores a device JWT the way a paired runner holds
//! one and calls them, so it observes the operator's bearer exactly where the
//! bearer would leave the runner.
//!
//! These tests live in a file of their own, with their own recorders, so that a
//! whole-file revert of `sdk_client.rs` (the "without the change" run of the
//! arming record) keeps them.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::mcp::adb_helper::{forward_remove, forward_tcp, refresh_adb_listed_forwarders};
use crate::mcp::physical_device_api::connect_device_at;
use crate::mcp::sdk_client::{
    connect_or_reuse, connect_sdk_app, recheck_stored_bearers, ConnectHints, ConnectResponse,
    SdkConnectionManager,
};
use crate::mcp::transport::cloud::CloudTransport;
use crate::mcp::transport::lan::LanTransport;
use crate::mcp::transport::{is_off_machine_forwarder, ForgetForwarder};

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
    _adb_port: crate::test_env::EnvVarRestore,
    _no_adb: tokio::net::TcpSocket,
    _ambient: crate::test_env::IsolatedAmbient,
}

/// Enter the door ambient:
/// - [`crate::test_env::isolated_ambient`] supplies the env lock and an empty
///   config and secure-storage dir;
/// - the keychain is switched off, so `store_tokens` never writes the
///   operator's real one;
/// - every proxy variable is cleared, so a proxy in the developer's shell is
///   never handed a request, and `NO_PROXY` is then set to exactly what the
///   runner exports at startup (`outbound_net::LOOPBACK_NO_PROXY`). The
///   tests below point the process-global `HTTP_PROXY` at a recording proxy,
///   and every OTHER test running in this process at that moment builds its
///   clients from the same environment: with `NO_PROXY` empty, their
///   `http://127.0.0.1` requests went through the recording proxy and failed
///   (`forward_claims_get_unreachable_coord_is_502` read an empty 502 body).
///   With the runner's own `NO_PROXY` they bypass it, as they do in
///   production;
/// - the adb server is a port nothing listens on, so a connect never reads the
///   developer's real adb into the process-global list;
/// - an unexpired device JWT is stored where `connect_sdk_app` will find it.
fn door() -> Door {
    let ambient = crate::test_env::isolated_ambient();
    // Restored by the fixture: the key is in `ambient::AMBIENT_ENV_KEYS`.
    std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
    let proxies = crate::test_env::EnvVarRestore::capture(&PROXY_ENV);
    for key in PROXY_ENV {
        std::env::remove_var(key);
    }
    std::env::set_var(
        "NO_PROXY",
        qontinui_runner_lib::outbound_net::LOOPBACK_NO_PROXY.join(","),
    );
    let adb_port = crate::test_env::EnvVarRestore::capture(&[
        "ANDROID_ADB_SERVER_PORT",
        "ANDROID_ADB_SERVER_ADDRESS",
        "ADB_SERVER_SOCKET",
    ]);
    std::env::remove_var("ANDROID_ADB_SERVER_ADDRESS");
    std::env::remove_var("ADB_SERVER_SOCKET");
    let (no_adb, no_adb_port) = a_port_nothing_listens_on();
    std::env::set_var("ANDROID_ADB_SERVER_PORT", no_adb_port.to_string());
    crate::auth::AuthManager::new()
        .store_tokens(&a_stored_device_jwt(), "")
        .expect("the fixture stores a device JWT");
    Door {
        _proxies: proxies,
        _adb_port: adb_port,
        _no_adb: no_adb,
        _ambient: ambient,
    }
}

/// A loopback port that refuses connections. The socket holds it, bound but not
/// listening, so no peer test can be handed the number.
fn a_port_nothing_listens_on() -> (tokio::net::TcpSocket, u16) {
    let socket = tokio::net::TcpSocket::new_v4().expect("socket");
    socket.bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let port = socket.local_addr().expect("local addr").port();
    (socket, port)
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
    spawn_app_on("127.0.0.1").await
}

/// [`spawn_app`] on a chosen loopback address.
async fn spawn_app_on(ip: &str) -> (u16, Seen) {
    let seen: Seen = Arc::default();
    // Hold the same port on 127.0.0.1 as well: forwarder marks go by port, so a
    // peer test handed this number there would read as this app, or the reverse.
    let (listener, port, held) = loop {
        let listener = TcpListener::bind((ip, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        if ip == "127.0.0.1" {
            break (listener, port, None);
        }
        if let Ok(held) = std::net::TcpListener::bind(("127.0.0.1", port)) {
            break (listener, port, Some(held));
        }
    };
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
        let _held = held;
        let _ = axum::serve(listener, app).await;
    });
    (port, seen)
}

/// A forward proxy on 127.0.0.1, set through `HTTP_PROXY`. It records the
/// target authority and the `Authorization` of every request head it receives.
/// - A request for `stand_in` is answered by the proxy itself, as a healthy app.
/// - Any other loopback destination is spliced straight through.
/// - Anything else is refused with 502, so the proxy never dials off the
///   machine.
///
/// `HTTP_PROXY` is process-global, so a test running alongside may build a
/// client while it points here and keep using that client after this test
/// ends. The proxy therefore runs on its own thread and runtime and lives for
/// the rest of the test process, splicing that test's loopback traffic to its
/// own server instead of resetting it.
fn spawn_proxy(stand_in: String) -> (u16, Seen) {
    let seen: Seen = Arc::default();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let seen_by_proxy = seen.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the proxy's own runtime");
        runtime.block_on(proxy_loop(listener, stand_in, seen_by_proxy));
    });
    (port, seen)
}

/// [`spawn_proxy`]'s accept loop.
async fn proxy_loop(listener: std::net::TcpListener, stand_in: String, seen: Seen) {
    let listener = TcpListener::from_std(listener).expect("a non-blocking std listener");
    while let Ok((mut client, _)) = listener.accept().await {
        let seen = seen.clone();
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
    let (proxy_port, seen) = spawn_proxy("foreign.invalid:80".to_string());
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
/// The relay is at `127.0.0.2`: a loopback address the runner's `NO_PROXY`
/// (`127.0.0.1`, `::1`, `localhost`) does not cover, though the bearer's
/// loopback class admits all of `127.0.0.0/8`. So the only thing that keeps
/// the request off the proxy is the connect's own client. The loopback relay
/// is admitted, so it does get the bearer, and gets it directly.
///
/// Not on macOS, which answers only `127.0.0.1` on its loopback interface
/// unless an alias is configured.
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_configured_proxy_never_sees_a_loopback_connect() {
    let _door = door();
    let (app_port, app_seen) = spawn_app_on("127.0.0.2").await;
    let (proxy_port, proxy_seen) = spawn_proxy(String::new());
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let relay = format!("127.0.0.2:{app_port}");
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

/// A manually registered LAN device is dialled at a local TCP proxy, so its URL
/// reads as loopback while the bytes reach the device. It never receives the
/// operator's bearer, and the device connect door says so. The recorder plays
/// the device: it is on loopback here only because a test has no LAN.
#[tokio::test]
async fn a_lan_proxy_never_carries_the_stored_bearer_to_the_device() {
    let _door = door();
    let (device_port, seen) = spawn_app().await;
    let (proxy_port, _proxy) = LanTransport::new()
        .create_proxy(std::net::SocketAddr::from(([127, 0, 0, 1], device_port)))
        .await
        .expect("the LAN proxy starts");
    let _forget = ForgetForwarder(proxy_port);
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let authority = format!("127.0.0.1:{proxy_port}");

    let connected = connect_device_at(
        &manager,
        "lan-under-test".into(),
        format!("http://{authority}"),
    )
    .await
    .unwrap_or_else(|e| panic!("the device connect failed: {:?}", e.error));

    assert_eq!(
        serde_json::to_value(&connected).unwrap()["authPath"],
        "withheld_foreign_host",
        "the door's response names the bearer decision"
    );
    let probes = requests_to(&seen, &authority);
    assert!(
        !probes.is_empty(),
        "the handshake never reached the device through the proxy"
    );
    assert!(
        probes.iter().all(Option::is_none),
        "the LAN proxy carried the operator's stored bearer to the device: {probes:?}"
    );
}

/// A failed device connect names the bearer decision in its error envelope, as
/// `handle_connect` does. The proxy's far end hangs up on every connection, and
/// stays bound so no peer test can be handed the port.
#[tokio::test]
async fn a_failed_device_connect_says_the_bearer_was_withheld() {
    let _door = door();
    let hangs_up = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hangs_up_at = hangs_up.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = hangs_up.accept().await {
            drop(stream);
        }
    });
    let (proxy_port, _proxy) = LanTransport::new()
        .create_proxy(hangs_up_at)
        .await
        .expect("the LAN proxy starts");
    let _forget = ForgetForwarder(proxy_port);
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let Err(envelope) = connect_device_at(
        &manager,
        "lan-under-test".into(),
        format!("http://127.0.0.1:{proxy_port}"),
    )
    .await
    else {
        panic!("a connect to a device that hangs up must fail");
    };

    assert_eq!(
        envelope.hint,
        Some(serde_json::json!({ "authPath": "withheld_foreign_host" }))
    );
}

/// A Cloud tunnel copies every request header into the `tunnel_request` it sends
/// to the relay, which delivers it to the device. The tunnel authenticates with
/// its own token, so the operator's bearer must not be among those headers. The
/// relay here answers each request like a healthy device.
#[tokio::test]
async fn a_cloud_tunnel_never_carries_the_stored_bearer_to_the_relay() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let _door = door();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_port = listener.local_addr().unwrap().port();
    let seen_headers = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let recorder = seen_headers.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let request: serde_json::Value = serde_json::from_str(&text).unwrap();
            if request["type"] != "tunnel_request" {
                continue;
            }
            recorder.lock().unwrap().push(request["headers"].clone());
            let response = serde_json::json!({
                "type": "tunnel_response",
                "id": request["id"],
                "status": 200,
                "headers": { "content-type": "application/json" },
                "body": serde_json::from_str::<serde_json::Value>(HEALTH).unwrap(),
            });
            ws.send(Message::Text(response.to_string().into()))
                .await
                .unwrap();
        }
    });

    let transport = CloudTransport::new(format!("http://127.0.0.1:{relay_port}"));
    let tunnel_port = transport
        .open_tunnel("device-under-test", "tunnel-token")
        .await
        .expect("the tunnel opens against the local relay");
    let _forget = ForgetForwarder(tunnel_port);
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let connected = connect_device_at(
        &manager,
        "cloud-under-test".into(),
        format!("http://127.0.0.1:{tunnel_port}"),
    )
    .await
    .unwrap_or_else(|e| panic!("the device connect failed: {:?}", e.error));

    assert_eq!(
        serde_json::to_value(&connected).unwrap()["authPath"],
        "withheld_foreign_host"
    );
    let headers = seen_headers.lock().unwrap().clone();
    assert!(
        !headers.is_empty(),
        "the handshake never reached the relay through the tunnel"
    );
    assert!(
        headers.iter().all(|h| h.get("authorization").is_none()),
        "the tunnel carried the operator's stored bearer to the relay: {headers:?}"
    );
}

/// A forwarder is dialled direct like any loopback relay: a configured proxy sees
/// neither the connect nor the request, and the far end gets no bearer. The port
/// is at `127.0.0.2`, which the runner's `NO_PROXY` does not cover, as in
/// [`a_configured_proxy_never_sees_a_loopback_connect`].
///
/// Not on macOS, for the same reason as that test.
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_configured_proxy_never_sees_a_forwarder_connect() {
    let _door = door();
    let (forwarder_port, forwarder_seen) = spawn_app_on("127.0.0.2").await;
    crate::mcp::transport::mark_off_machine_forwarder(forwarder_port);
    let _forget = ForgetForwarder(forwarder_port);
    let (proxy_port, proxy_seen) = spawn_proxy(String::new());
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let authority = format!("127.0.0.2:{forwarder_port}");
    std::env::set_var("HTTP_PROXY", format!("http://127.0.0.1:{proxy_port}"));
    let connected = connect_sdk_app(
        &manager,
        &format!("http://{authority}"),
        None,
        None,
        None,
        None,
        None,
    )
    .await;
    std::env::remove_var("HTTP_PROXY");

    let via_proxy = requests_to(&proxy_seen, &authority);
    assert!(
        via_proxy.is_empty(),
        "a forwarder connect went through the proxy: {via_proxy:?}"
    );
    let direct = requests_to(&forwarder_seen, &authority);
    assert!(
        connected.is_ok() && !direct.is_empty(),
        "the forwarder was not reached directly: {direct:?}"
    );
    assert!(
        direct.iter().all(Option::is_none),
        "a forwarder received the operator's stored bearer: {direct:?}"
    );
}

/// Points `adb_helper` at a fake adb server until dropped, and leaves no listed
/// forward behind. Hold the [`door`] lock around it: the variable is global.
struct FakeAdb {
    _port: crate::test_env::EnvVarRestore,
}

impl FakeAdb {
    /// A server that answers `host:list-forward` with `list`, and `OKAY` to any
    /// other request.
    async fn start(list: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let list = list.clone();
                tokio::spawn(async move {
                    let mut length = [0u8; 4];
                    while stream.read_exact(&mut length).await.is_ok() {
                        let length = std::str::from_utf8(&length).unwrap();
                        let mut request = vec![0u8; usize::from_str_radix(length, 16).unwrap()];
                        stream.read_exact(&mut request).await.unwrap();
                        let reply = if request == b"host:list-forward" {
                            format!("OKAY{:04x}{list}", list.len())
                        } else {
                            "OKAY".to_string()
                        };
                        stream.write_all(reply.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
        std::env::set_var("ANDROID_ADB_SERVER_PORT", port.to_string());
        Self { _port: restore }
    }
}

impl Drop for FakeAdb {
    fn drop(&mut self) {
        use crate::mcp::transport::{next_adb_list_read, set_adb_listed_forwarders};
        set_adb_listed_forwarders(next_adb_list_read(), Default::default());
    }
}

/// A forward this process never opened still reaches the phone: the adb server
/// outlives the runner, and an operator can run `adb forward` by hand. The adb
/// server's own list keeps the bearer off it, and a local emulator's forward
/// keeps the bearer.
#[tokio::test]
async fn a_forward_on_the_adb_servers_list_never_carries_the_stored_bearer() {
    let _door = door();
    let (phone_port, phone_seen) = spawn_app().await;
    let (emulator_port, emulator_seen) = spawn_app().await;
    let _adb = FakeAdb::start(format!(
        "R3CN30ABCDEF tcp:{phone_port} tcp:9876\nemulator-5554 tcp:{emulator_port} tcp:9876\n"
    ))
    .await;
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    for port in [phone_port, emulator_port] {
        connect_sdk_app(
            &manager,
            &format!("http://127.0.0.1:{port}"),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    }

    let phone = requests_to(&phone_seen, &format!("127.0.0.1:{phone_port}"));
    assert!(!phone.is_empty(), "the handshake never reached the phone");
    assert!(
        phone.iter().all(Option::is_none),
        "an adb forward carried the operator's stored bearer to the phone: {phone:?}"
    );
    let emulator = requests_to(&emulator_seen, &format!("127.0.0.1:{emulator_port}"));
    assert!(
        !emulator.is_empty() && emulator.iter().all(Option::is_some),
        "a local emulator's forward lost the bearer: {emulator:?}"
    );
}

/// Point the adb client at a server that answers `FAIL` to everything.
async fn start_unreadable_adb() {
    let bad = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bad_port = bad.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = bad.accept().await {
            let _ = stream.write_all(b"FAIL").await;
        }
    });
    std::env::set_var("ANDROID_ADB_SERVER_PORT", bad_port.to_string());
}

/// A server that is not there holds no forwards, so its old list goes and the list
/// is known. A server that answers unreadably, or not at all, says nothing: the old
/// list stays and the list is not known.
#[tokio::test]
async fn the_adb_list_goes_with_the_server_but_not_with_a_bad_reply() {
    let _door = door();
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = held.local_addr().expect("local addr").port();
    let _adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
    assert!(refresh_adb_listed_forwarders().await);
    assert!(is_off_machine_forwarder(port));

    start_unreadable_adb().await;
    assert!(!refresh_adb_listed_forwarders().await, "a bad reply");
    assert!(is_off_machine_forwarder(port), "a bad reply kept the list");

    let _garbled = FakeAdb::start("garbled\n".to_string()).await;
    assert!(!refresh_adb_listed_forwarders().await, "a garbled list");
    assert!(is_off_machine_forwarder(port), "a garbled list kept it");

    // Accepted by the backlog and never answered.
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    std::env::set_var("ANDROID_ADB_SERVER_PORT", silent_port.to_string());
    assert!(!refresh_adb_listed_forwarders().await, "no answer");
    assert!(is_off_machine_forwarder(port), "no answer kept the list");

    let (_absent, absent_port) = a_port_nothing_listens_on();
    std::env::set_var("ANDROID_ADB_SERVER_PORT", absent_port.to_string());
    assert!(refresh_adb_listed_forwarders().await, "no server is known");
    assert!(!is_off_machine_forwarder(port), "no server, no forwards");
}

/// While the adb server cannot be read, any loopback port might be a forward to a
/// phone, so a connect sends no bearer and says why. Once it can be read, the stored
/// bearerless connection gives way to a handshake that decides afresh.
#[tokio::test]
async fn an_unreadable_adb_list_withholds_the_stored_bearer_until_it_can_be_read() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
    start_unreadable_adb().await;
    let connect = || connect_sdk_app(&manager, &url, None, None, None, None, None);
    let withheld = connect()
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    assert_eq!(withheld.auth_path, "withheld_forward_list_unreadable");
    let authority = format!("127.0.0.1:{port}");
    let requests = requests_to(&seen, &authority);
    assert!(!requests.is_empty(), "the handshake never reached the app");
    assert!(
        requests.iter().all(Option::is_none),
        "an unreadable adb list let the bearer through: {requests:?}"
    );
    drop(restore);

    assert!(refresh_adb_listed_forwarders().await);
    assert!(
        manager
            .lock()
            .await
            .reuse_stored_connection(&url, true)
            .is_none(),
        "the bearerless client must give way to a new handshake"
    );
    let decided = connect()
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    assert_ne!(decided.auth_path, "withheld_forward_list_unreadable");
    assert!(
        requests_to(&seen, &authority)
            .last()
            .is_some_and(Option::is_some),
        "the readable list did not bring the bearer back"
    );
}

/// The whole of `POST /ui-bridge/sdk/connect`, from one stored bearer client: it is
/// reused while the list reads, given up for a bearerless handshake once the list
/// cannot be read or names the port, and brought back once the port is known local.
#[tokio::test]
async fn the_connect_route_decides_the_bearer_afresh_whenever_the_list_changes_its_answer() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let authority = format!("127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let connect = || async {
        connect_or_reuse(&manager, &url, ConnectHints::default())
            .await
            .unwrap_or_else(|e| panic!("the connect failed: {}", e.message))
    };
    let last_bearer = |response: &ConnectResponse| {
        let last = requests_to(&seen, &authority).last().cloned().flatten();
        (response.auth_path.clone(), last.is_some())
    };

    let first = connect().await;
    assert_ne!(first.auth_path, "reused");
    assert!(last_bearer(&first).1, "a local relay gets the bearer");
    assert_eq!(connect().await.auth_path, "reused");

    let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
    start_unreadable_adb().await;
    let unreadable = connect().await;
    assert_eq!(
        last_bearer(&unreadable),
        ("withheld_forward_list_unreadable".to_string(), false)
    );
    drop(restore);

    let recovered = connect().await;
    assert_ne!(recovered.auth_path, "reused");
    assert!(last_bearer(&recovered).1, "the readable list restores it");

    {
        let _adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
        let forwarded = connect().await;
        assert_ne!(forwarded.auth_path, "reused");
        assert!(!last_bearer(&forwarded).1, "a listed port gets no bearer");
    }
}

/// What the runner re-runs every 30s. An unreadable list costs no stored
/// connection, or its target. A connection that held the bearer back gets it once the
/// list reads, with no handshake. One whose port the list names is dropped.
#[tokio::test]
async fn the_recheck_settles_stored_connections_and_keeps_their_target() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let authority = format!("127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let connect = || connect_sdk_app(&manager, &url, None, None, None, None, None);
    let last_request_has_bearer = || async {
        let manager = manager.lock().await;
        let conn = manager.active_connection().expect("still the target");
        conn.client.get(&url).send().await.expect("a request");
        requests_to(&seen, &authority)
            .last()
            .is_some_and(Option::is_some)
    };

    connect()
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
    start_unreadable_adb().await;
    recheck_stored_bearers(&manager).await;
    assert!(
        last_request_has_bearer().await,
        "an unreadable list keeps a stored connection as it is"
    );

    connect()
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    assert!(
        !last_request_has_bearer().await,
        "withheld while unreadable"
    );
    recheck_stored_bearers(&manager).await;
    assert!(!last_request_has_bearer().await, "still unreadable");
    drop(restore);

    recheck_stored_bearers(&manager).await;
    assert!(
        last_request_has_bearer().await,
        "a readable list brings the bearer back to the same target"
    );
    assert_eq!(manager.lock().await.active_url.as_deref(), Some(&*url));

    let _adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
    recheck_stored_bearers(&manager).await;
    let manager = manager.lock().await;
    assert!(!manager.connections.contains_key(&url));
    assert_eq!(manager.active_url, None);
}

/// A forward that goes for a moment, as one does when a cable is pulled, must not
/// leave a bearer client behind for when it comes back. A connection withheld from a
/// listed port stays withheld until a handshake reads the list again.
#[tokio::test]
async fn a_forward_that_comes_back_finds_no_bearer_client_waiting() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let authority = format!("127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());
    let list = format!("R3CN30ABCDEF tcp:{port} tcp:9876\n");

    let adb = FakeAdb::start(list.clone()).await;
    connect_sdk_app(&manager, &url, None, None, None, None, None)
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    drop(adb);

    recheck_stored_bearers(&manager).await;
    let _adb = FakeAdb::start(list.clone()).await;
    assert!(refresh_adb_listed_forwarders().await);

    let manager = manager.lock().await;
    let conn = manager.active_connection().expect("still the target");
    conn.client.get(&url).send().await.expect("a request");
    let requests = requests_to(&seen, &authority);
    assert!(
        requests.iter().all(Option::is_none),
        "the bearer reached a forward that came back: {requests:?}"
    );
}

/// A connection withheld only because the list was unreadable is withheld for what
/// its port is once the list names it, so its going later hands it no bearer.
#[tokio::test]
async fn a_list_that_reads_again_and_names_the_port_settles_the_withholding() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let authority = format!("127.0.0.1:{port}");
    let manager = tokio::sync::Mutex::new(SdkConnectionManager::new());

    let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
    start_unreadable_adb().await;
    connect_sdk_app(&manager, &url, None, None, None, None, None)
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    drop(restore);

    let adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
    recheck_stored_bearers(&manager).await;
    drop(adb);
    recheck_stored_bearers(&manager).await;

    let manager = manager.lock().await;
    let conn = manager.active_connection().expect("still the target");
    conn.client.get(&url).send().await.expect("a request");
    assert!(
        requests_to(&seen, &authority).iter().all(Option::is_none),
        "a port the list once named got the bearer with no handshake"
    );
}

/// `ADB_SERVER_SOCKET` wins over the port variable, as it does for `adb`, and one
/// that names no server on loopback leaves the list unknown rather than empty.
#[tokio::test]
async fn the_adb_list_is_read_from_the_server_the_environment_names() {
    let _door = door();
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = held.local_addr().expect("local addr").port();
    let _adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
    let adb_port = std::env::var("ANDROID_ADB_SERVER_PORT").unwrap();
    let (_absent, absent_port) = a_port_nothing_listens_on();
    std::env::set_var("ANDROID_ADB_SERVER_PORT", absent_port.to_string());
    std::env::set_var("ADB_SERVER_SOCKET", format!("tcp:127.0.0.1:{adb_port}"));
    assert!(refresh_adb_listed_forwarders().await);
    assert!(is_off_machine_forwarder(port));

    std::env::set_var("ADB_SERVER_SOCKET", "localfilesystem:/tmp/adb");
    assert!(!refresh_adb_listed_forwarders().await);
    assert!(is_off_machine_forwarder(port), "the last list stays");
}

/// A switch while the list cannot be read refuses a stored bearer client, and leaves
/// a stored bearerless one in use, whose decision the unreadable list cannot fault.
#[tokio::test]
async fn a_switch_on_an_unreadable_list_refuses_only_a_bearer_client() {
    let _door = door();
    let (port, _seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let relay = crate::mcp::relay_binding::RelayState::standalone(Default::default());
    let switch = || {
        crate::mcp::sdk_client::handle_switch(
            axum::extract::State(relay.clone()),
            axum::Json(serde_json::from_value(serde_json::json!({ "url": url })).unwrap()),
        )
    };
    connect_sdk_app(&relay.sdk_connection, &url, None, None, None, None, None)
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));

    let restore = crate::test_env::EnvVarRestore::capture(&["ANDROID_ADB_SERVER_PORT"]);
    start_unreadable_adb().await;
    let axum::Json(refused) = switch().await;
    assert!(
        refused.error.expect("refused").contains("no longer holds"),
        "a bearer client must not be reused on an unreadable list"
    );

    let withheld = connect_sdk_app(&relay.sdk_connection, &url, None, None, None, None, None)
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    assert_eq!(withheld.auth_path, "withheld_forward_list_unreadable");
    let axum::Json(reused) = switch().await;
    assert_eq!(reused.data.expect("reused").auth_path, "reused");
    drop(restore);
}

/// `POST /ui-bridge/sdk/switch` reads the adb server's list before it reuses a stored
/// client, so a forward made by hand over that port since the connect is seen in time.
#[tokio::test]
async fn a_switch_never_reuses_a_bearer_client_over_a_forward_made_since_the_connect() {
    let _door = door();
    let (port, seen) = spawn_app().await;
    let url = format!("http://127.0.0.1:{port}");
    let relay = crate::mcp::relay_binding::RelayState::standalone(Default::default());
    connect_sdk_app(&relay.sdk_connection, &url, None, None, None, None, None)
        .await
        .unwrap_or_else(|e| panic!("the connect failed: {}", e.message));
    let authority = format!("127.0.0.1:{port}");
    assert!(requests_to(&seen, &authority).iter().all(Option::is_some));

    let _adb = FakeAdb::start(format!("R3CN30ABCDEF tcp:{port} tcp:9876\n")).await;
    let axum::Json(answer) = crate::mcp::sdk_client::handle_switch(
        axum::extract::State(relay.clone()),
        axum::Json(serde_json::from_value(serde_json::json!({ "url": url })).unwrap()),
    )
    .await;

    let error = answer.error.expect("the switch is refused");
    assert!(error.contains("no longer holds"), "{error}");
    assert!(!relay
        .sdk_connection
        .lock()
        .await
        .connections
        .contains_key(&url));
}

/// `forward_tcp` and `forward_remove` keep the mark in step with the forward for
/// a phone, and make none for a local emulator. The port is held bound so no peer
/// test can be handed it.
#[tokio::test]
async fn an_adb_forward_is_marked_for_as_long_as_it_exists() {
    let _door = door();
    let _adb = FakeAdb::start(String::new()).await;
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = held.local_addr().expect("local addr").port();
    let _forget = ForgetForwarder(port);

    forward_tcp("emulator-5554".into(), port, 9876)
        .await
        .expect("the fake adb accepts the forward");
    assert!(!is_off_machine_forwarder(port), "an emulator's stays local");

    forward_tcp("R3CN30ABCDEF".into(), port, 9876)
        .await
        .expect("the fake adb accepts the forward");
    assert!(is_off_machine_forwarder(port), "a phone's is marked");

    forward_remove("emulator-5554".into(), port)
        .await
        .expect("the fake adb accepts the removal");
    assert!(
        is_off_machine_forwarder(port),
        "an emulator's removal drops no mark"
    );

    forward_remove("R3CN30ABCDEF".into(), port)
        .await
        .expect("the fake adb accepts the removal");
    assert!(!is_off_machine_forwarder(port), "a removed forward is gone");
}
