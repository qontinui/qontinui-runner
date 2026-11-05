//! A scripted local coord double for the acquire tests: a bare HTTP/1.1
//! responder on `127.0.0.1:0` that answers each route prefix from its own
//! queue of `(status, body)` and records what it was sent. When a route's
//! queue runs down to its last entry, that entry repeats. An unscripted path
//! answers 404.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Script = Vec<(&'static str, Vec<(u16, String)>)>;

#[derive(Default)]
struct State {
    queues: Vec<(&'static str, Vec<(u16, String)>)>,
    hits: HashMap<&'static str, usize>,
    bodies: HashMap<&'static str, Vec<serde_json::Value>>,
    paths: HashMap<&'static str, Vec<String>>,
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
                    let Some((path, body)) = read_request(&mut sock).await else {
                        return;
                    };
                    let (status, text) = respond(&st, &path, &body);
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
}

fn respond(state: &Arc<Mutex<State>>, path: &str, body: &[u8]) -> (u16, String) {
    let mut st = state.lock().unwrap();
    let Some(idx) = st.queues.iter().position(|(p, _)| path.starts_with(p)) else {
        return (404, "{}".to_string());
    };
    let prefix = st.queues[idx].0;
    *st.hits.entry(prefix).or_insert(0) += 1;
    st.bodies
        .entry(prefix)
        .or_default()
        .push(serde_json::from_slice(body).unwrap_or(serde_json::Value::Null));
    st.paths.entry(prefix).or_default().push(path.to_string());
    let queue = &mut st.queues[idx].1;
    if queue.len() > 1 {
        queue.remove(0)
    } else {
        queue.first().cloned().unwrap_or((404, "{}".to_string()))
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
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
    let len = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = (head_end + len).min(buf.len());
    Some((path, buf[head_end..end].to_vec()))
}
