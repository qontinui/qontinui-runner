//! A scripted local coord double: a bare HTTP/1.1 responder on `127.0.0.1:0`
//! that answers each route prefix from its own queue of `(status, body)` and
//! records what it was sent — the path, the JSON body and the `Authorization`
//! header of every request. When a route's queue runs down to its last entry,
//! that entry repeats. An unscripted path answers 404.
//!
//! [`coord_fixture`] is the ONE place a test satisfies the three ambient
//! prerequisites every fleet publisher returns early without
//! (`fleet::load_device_file`, `profiles::connected_coord_base`,
//! `coord_mcp::read_usable_device_jwt`), so a test can enter
//! `resource_sample::publish_once` and `computer::Reporter::tick` the way the
//! sampler loop does, against this double. Plan
//! `2026-11-15-fleet-publishers-loopback-coord-harness-and-detect-environment-race`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Script = Vec<(&'static str, Vec<(u16, String)>)>;

/// The `device_id` the fixture's `machine.json` states.
pub(crate) const FIXTURE_DEVICE_ID: &str = "fixture-device";

/// The `hostname` the fixture's `machine.json` states.
pub(crate) const FIXTURE_HOSTNAME: &str = "fixture-host";

/// Every variable reqwest's system proxy reads, in both spellings.
pub(crate) const PROXY_ENV: [&str; 8] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

#[derive(Default)]
struct State {
    queues: Vec<(&'static str, Vec<(u16, String)>)>,
    hits: HashMap<&'static str, usize>,
    bodies: HashMap<&'static str, Vec<serde_json::Value>>,
    paths: HashMap<&'static str, Vec<String>>,
    authorizations: HashMap<&'static str, Vec<Option<String>>>,
}

/// One request as the double read it off the socket.
struct Request {
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

/// The ambient fixture every test against this double holds: the env lock, a
/// temp `QONTINUI_HOME`, the keychain off (so no operator credential is read or
/// sent to the double), and worktree mode on whatever the box's environment
/// says. Both keys are on the fixture's capture list and are restored on drop.
pub(crate) fn isolate() -> crate::test_env::IsolatedAmbient {
    let amb = crate::test_env::isolated_ambient();
    std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
    std::env::set_var("QONTINUI_AGENT_WORKTREE_MODE", "1");
    amb
}

/// Clear every proxy variable and set `NO_PROXY` to exactly what the runner
/// exports at startup (`outbound_net::LOOPBACK_NO_PROXY`), so a proxy in the
/// developer's shell is never handed a request and loopback traffic bypasses
/// any proxy a sibling test points the process-global `HTTP_PROXY` at.
///
/// None of these keys is in `ambient::AMBIENT_ENV_KEYS`, so the returned guard
/// is what restores them; hold it inside the env lock and drop it first.
pub(crate) fn loopback_only_proxies() -> crate::test_env::EnvVarRestore {
    let proxies = crate::test_env::EnvVarRestore::capture(&PROXY_ENV);
    for key in PROXY_ENV {
        std::env::remove_var(key);
    }
    std::env::set_var(
        "NO_PROXY",
        qontinui_runner_lib::outbound_net::LOOPBACK_NO_PROXY.join(","),
    );
    proxies
}

/// A loopback port that refuses connections. The socket holds it, bound but not
/// listening, so no peer test can be handed the number.
pub(crate) fn a_port_nothing_listens_on() -> (tokio::net::TcpSocket, u16) {
    let socket = tokio::net::TcpSocket::new_v4().expect("socket");
    socket.bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let port = socket.local_addr().expect("local addr").port();
    (socket, port)
}

/// An unexpired, JWT-shaped device token. It is unsigned: nothing on the
/// runner's bearer path verifies a signature, only the token's shape and its
/// `exp`.
pub(crate) fn a_stored_device_jwt() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let exp = chrono::Utc::now().timestamp() + 3600;
    let payload =
        URL_SAFE_NO_PAD.encode(serde_json::json!({ "sub_type": "device", "exp": exp }).to_string());
    format!("{header}.{payload}.sig")
}

/// The ambient a fleet-publisher test runs in. Fields drop in declaration
/// order: the proxy variables are restored while the env lock is still held,
/// and the ambient (which holds the lock) goes last.
pub(crate) struct CoordFixture {
    _proxies: crate::test_env::EnvVarRestore,
    /// The device JWT stored in the fixture's secure storage — what a
    /// publisher must send as `Authorization: Bearer <bearer>`.
    pub(crate) bearer: String,
    pub(crate) ambient: crate::test_env::IsolatedAmbient,
}

impl CoordFixture {
    /// Point `$COORD_HTTP_URL` at `base` (a started [`ScriptedCoord`]'s). The
    /// key is captured by the ambient fixture, so it is restored on drop.
    pub(crate) fn connect(&self, base: &str) {
        std::env::set_var("COORD_HTTP_URL", base);
    }
}

/// Satisfy the publishers' three prerequisites inside an isolated ambient:
/// - [`isolate`] (the env lock, an empty `QONTINUI_HOME`, the keychain off);
/// - a `machine.json` stating [`FIXTURE_DEVICE_ID`] and [`FIXTURE_HOSTNAME`],
///   which `fleet::load_device_file` requires both of;
/// - an unexpired device JWT stored in the fixture's secure storage, which
///   `coord_mcp::read_usable_device_jwt` returns;
/// - proxies cleared ([`loopback_only_proxies`]), so the POST reaches the
///   double directly.
///
/// The coord base is the caller's: once a [`ScriptedCoord`] is started,
/// [`CoordFixture::connect`] points `$COORD_HTTP_URL` at it.
pub(crate) fn coord_fixture() -> CoordFixture {
    let ambient = isolate();
    let proxies = loopback_only_proxies();
    ambient.write_machine_json(
        &serde_json::json!({
            "device_id": FIXTURE_DEVICE_ID,
            "hostname": FIXTURE_HOSTNAME,
        })
        .to_string(),
    );
    let bearer = a_stored_device_jwt();
    crate::auth::AuthManager::new()
        .store_tokens(&bearer, "")
        .expect("the fixture stores a device JWT");
    CoordFixture {
        _proxies: proxies,
        bearer,
        ambient,
    }
}

pub(crate) struct ScriptedCoord {
    pub(crate) base: String,
    state: Arc<Mutex<State>>,
}

impl ScriptedCoord {
    pub(crate) async fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(State {
            queues: script,
            ..State::default()
        }));
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let st = st.clone();
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut sock).await else {
                        return;
                    };
                    let (status, text) = respond(&st, req);
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{text}",
                        text.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self {
            base: format!("http://{addr}"),
            state,
        }
    }

    /// Requests received on `prefix` (the script's key, verbatim).
    pub(crate) fn hits(&self, prefix: &str) -> usize {
        let st = self.state.lock().unwrap();
        st.hits
            .iter()
            .find(|(k, _)| **k == prefix)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    /// JSON bodies received on `prefix`, in order.
    pub(crate) fn bodies(&self, prefix: &str) -> Vec<serde_json::Value> {
        let st = self.state.lock().unwrap();
        st.bodies
            .iter()
            .find(|(k, _)| **k == prefix)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    /// Request paths received on `prefix`, in order.
    pub(crate) fn paths(&self, prefix: &str) -> Vec<String> {
        let st = self.state.lock().unwrap();
        st.paths
            .iter()
            .find(|(k, _)| **k == prefix)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    /// The `Authorization` header of each request received on `prefix`, in
    /// order (`None` where a request carried none).
    pub(crate) fn authorizations(&self, prefix: &str) -> Vec<Option<String>> {
        let st = self.state.lock().unwrap();
        st.authorizations
            .iter()
            .find(|(k, _)| **k == prefix)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }
}

fn respond(state: &Arc<Mutex<State>>, req: Request) -> (u16, String) {
    let mut st = state.lock().unwrap();
    let Some(idx) = st.queues.iter().position(|(p, _)| req.path.starts_with(p)) else {
        return (404, "{}".to_string());
    };
    let prefix = st.queues[idx].0;
    *st.hits.entry(prefix).or_insert(0) += 1;
    st.bodies
        .entry(prefix)
        .or_default()
        .push(serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null));
    st.paths.entry(prefix).or_default().push(req.path);
    st.authorizations
        .entry(prefix)
        .or_default()
        .push(req.authorization);
    let queue = &mut st.queues[idx].1;
    if queue.len() > 1 {
        queue.remove(0)
    } else {
        queue.first().cloned().unwrap_or((404, "{}".to_string()))
    }
}

/// The value of header `name` (case-insensitive) in a request head.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let path = head.split_whitespace().nth(1)?.to_string();
    let authorization = header(&head, "authorization").map(str::to_owned);
    let len = header(&head, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = (head_end + len).min(buf.len());
    Some(Request {
        path,
        authorization,
        body: buf[head_end..end].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The falsifier for the whole loopback harness: under [`coord_fixture`]
    /// and a started double, every prerequisite `publish_once` and
    /// `Reporter::tick` return early without is satisfied — by the fixture,
    /// not bypassed. If any of these three fails, a publisher test entering
    /// through those functions would skip before reaching the double.
    #[tokio::test]
    async fn fixture_satisfies_every_publisher_prerequisite() {
        let fx = coord_fixture();
        let coord = ScriptedCoord::start(vec![("/coord/", vec![(200, "{}".to_string())])]).await;
        fx.connect(&coord.base);

        let device = crate::fleet::load_device_file()
            .expect("the fixture's machine.json must satisfy load_device_file");
        // `load_device_file` answers `Some` only when BOTH the device id and
        // the hostname are present, so this also proves the hostname.
        assert_eq!(device.device_id, "fixture-device");
        assert_eq!(
            qontinui_runner_lib::profiles::connected_coord_base(),
            Some(coord.base.clone()),
            "$COORD_HTTP_URL must be the connected coord base under the fixture"
        );
        assert_eq!(
            crate::coord_mcp::read_usable_device_jwt().await,
            Some(fx.bearer.clone()),
            "the stored fixture JWT must be the usable device bearer"
        );
    }

    /// The double records the `Authorization` header per request, `None` for a
    /// request that carried none.
    #[tokio::test]
    async fn the_double_records_each_requests_authorization() {
        let _amb = isolate();
        let _proxies = loopback_only_proxies();
        let coord = ScriptedCoord::start(vec![("/x", vec![(200, "{}".to_string())])]).await;
        let client = reqwest::Client::new();
        client
            .post(format!("{}/x/1", coord.base))
            .bearer_auth("tok")
            .json(&serde_json::json!({"a": 1}))
            .send()
            .await
            .expect("send");
        client
            .get(format!("{}/x/2", coord.base))
            .send()
            .await
            .expect("send");
        assert_eq!(coord.hits("/x"), 2);
        assert_eq!(
            coord.authorizations("/x"),
            vec![Some("Bearer tok".to_string()), None]
        );
        assert_eq!(coord.paths("/x"), vec!["/x/1", "/x/2"]);
        assert_eq!(
            coord.bodies("/x"),
            vec![serde_json::json!({"a": 1}), serde_json::Value::Null]
        );
    }
}
