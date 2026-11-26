//! Loopback stand-ins for PostgreSQL, so the breaker's acceptance tests run
//! with no database (plan
//! `2026-10-19-runner-pg-unreachable-costs-every-caller-a-pool-timeout`,
//! Phase 1).
//!
//! [`PgStandIn`] binds `127.0.0.1:0` and serves one of four modes, switchable
//! at runtime on the SAME port so a recovery test needs no reconnect to a new
//! address:
//!
//! * [`Mode::Silent`] — accepts TCP and never writes: a hung handshake.
//! * [`Mode::Refuse`] — accepts and immediately drops the socket: a
//!   client-side connect failure with no server reply.
//! * [`Mode::Fake`] — a minimal protocol-3 responder: AuthenticationOk,
//!   ParameterStatus, BackendKeyData, ReadyForQuery, then simple queries
//!   (`SELECT 1` returns one row; anything else returns CommandComplete).
//! * [`Mode::AuthError`] — answers the startup packet with an ErrorResponse
//!   carrying SQLSTATE `28P01`: the server is REACHABLE and said no.
//!
//! [`closed_port`] returns a port with no listener at all.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Silent = 0,
    Refuse = 1,
    Fake = 2,
    AuthError = 3,
}

impl Mode {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Mode::Silent,
            1 => Mode::Refuse,
            2 => Mode::Fake,
            _ => Mode::AuthError,
        }
    }
}

/// A switchable loopback PostgreSQL stand-in. Dropping it stops the listener.
pub struct PgStandIn {
    port: u16,
    mode: Arc<AtomicU8>,
    accepts: Arc<AtomicU64>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for PgStandIn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PgStandIn {
    pub async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let mode_cell = Arc::new(AtomicU8::new(mode as u8));
        let accepts = Arc::new(AtomicU64::new(0));
        let (m, a) = (mode_cell.clone(), accepts.clone());
        let task = tokio::spawn(async move {
            // Silent sockets are parked here so they stay open (never
            // written, never closed) until the stand-in is dropped; served
            // connections live in a JoinSet, so dropping the stand-in (which
            // aborts this task) aborts them too.
            let mut parked: Vec<TcpStream> = Vec::new();
            let mut served = tokio::task::JoinSet::new();
            loop {
                while served.try_join_next().is_some() {}
                let sock = match listener.accept().await {
                    Ok((sock, _)) => sock,
                    // A client that aborted before the accept (the tests
                    // cancel connects all the time) is not a listener fault.
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue
                    }
                    // Anything else will not recover; stop rather than spin.
                    Err(_) => break,
                };
                a.fetch_add(1, Ordering::SeqCst);
                match Mode::from_u8(m.load(Ordering::SeqCst)) {
                    Mode::Silent => parked.push(sock),
                    Mode::Refuse => drop(sock),
                    Mode::Fake => {
                        served.spawn(serve_fake(sock));
                    }
                    Mode::AuthError => {
                        served.spawn(serve_auth_error(sock));
                    }
                }
            }
        });
        Self {
            port,
            mode: mode_cell,
            accepts,
            task,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn set_mode(&self, mode: Mode) {
        self.mode.store(mode as u8, Ordering::SeqCst);
    }

    /// TCP connections accepted so far (every mode counts).
    pub fn accepts(&self) -> u64 {
        self.accepts.load(Ordering::SeqCst)
    }

    pub fn url(&self) -> String {
        format!("postgres://fake:fake@127.0.0.1:{}/fake", self.port)
    }
}

/// A loopback port with no listener: bind, read the port, drop the listener.
pub fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    l.local_addr().expect("local addr").port()
}

/// A deadpool pool built the way `database::pg::PgDb::build_pool` builds one
/// (Fast recycling, Tokio1 runtime, bounded timeouts, the `search_path`
/// post-create hook), with test-sized timeouts and pool size. `create_ms`
/// bounds connect + startup and recycle; `wait_ms` bounds the wait for a slot.
pub fn test_pool(
    url: &str,
    create_ms: u64,
    wait_ms: u64,
    max_size: usize,
) -> deadpool_postgres::Pool {
    let pg_config: tokio_postgres::Config = url.parse().expect("test url parses");
    let mgr = deadpool_postgres::Manager::from_config(
        pg_config,
        tokio_postgres::NoTls,
        deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        },
    );
    let create = Some(std::time::Duration::from_millis(create_ms));
    deadpool_postgres::Pool::builder(mgr)
        .max_size(max_size)
        .runtime(deadpool_postgres::Runtime::Tokio1)
        .create_timeout(create)
        .wait_timeout(Some(std::time::Duration::from_millis(wait_ms)))
        .recycle_timeout(create)
        .post_create(deadpool_postgres::Hook::async_fn(|conn, _| {
            Box::pin(async move {
                conn.simple_query("SET search_path TO project, public")
                    .await
                    .map_err(|e| deadpool_postgres::HookError::Message(e.to_string().into()))?;
                Ok(())
            })
        }))
        .build()
        .expect("test pool builds")
}

fn msg(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(tag);
    out.extend_from_slice(&((body.len() as i32 + 4).to_be_bytes()));
    out.extend_from_slice(body);
    out
}

fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// Read the untagged startup packet (or an SSLRequest, answered `N`, followed
/// by the real startup packet). Returns `false` on EOF.
async fn read_startup(sock: &mut TcpStream) -> bool {
    loop {
        let mut len = [0u8; 4];
        if sock.read_exact(&mut len).await.is_err() {
            return false;
        }
        let n = i32::from_be_bytes(len).max(4) as usize - 4;
        let mut body = vec![0u8; n];
        if sock.read_exact(&mut body).await.is_err() {
            return false;
        }
        // 80877103 = SSLRequest; 80877104 = GSSENCRequest.
        let code = body
            .get(..4)
            .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0);
        if code == 80_877_103 || code == 80_877_104 {
            if sock.write_all(b"N").await.is_err() {
                return false;
            }
            continue;
        }
        return true;
    }
}

async fn serve_fake(mut sock: TcpStream) {
    if !read_startup(&mut sock).await {
        return;
    }
    let mut hello = msg(b'R', &0i32.to_be_bytes());
    for (k, v) in [
        ("server_version", "16.0"),
        ("client_encoding", "UTF8"),
        ("standard_conforming_strings", "on"),
    ] {
        let mut body = cstr(k);
        body.extend(cstr(v));
        hello.extend(msg(b'S', &body));
    }
    let mut key = 1234i32.to_be_bytes().to_vec();
    key.extend_from_slice(&5678i32.to_be_bytes());
    hello.extend(msg(b'K', &key));
    hello.extend(msg(b'Z', b"I"));
    if sock.write_all(&hello).await.is_err() {
        return;
    }
    loop {
        let mut tag = [0u8; 1];
        if sock.read_exact(&mut tag).await.is_err() {
            return;
        }
        let mut len = [0u8; 4];
        if sock.read_exact(&mut len).await.is_err() {
            return;
        }
        let n = i32::from_be_bytes(len).max(4) as usize - 4;
        let mut body = vec![0u8; n];
        if sock.read_exact(&mut body).await.is_err() {
            return;
        }
        match tag[0] {
            b'X' => return,
            b'Q' => {
                let q = String::from_utf8_lossy(&body);
                let q = q.trim_end_matches('\0').trim().to_ascii_uppercase();
                let mut out = Vec::new();
                if q == "SELECT 1" {
                    // RowDescription: 1 field "?column?", int4.
                    let mut rd = 1i16.to_be_bytes().to_vec();
                    rd.extend(cstr("?column?"));
                    rd.extend_from_slice(&0i32.to_be_bytes()); // table oid
                    rd.extend_from_slice(&0i16.to_be_bytes()); // attr number
                    rd.extend_from_slice(&23i32.to_be_bytes()); // int4 oid
                    rd.extend_from_slice(&4i16.to_be_bytes()); // type size
                    rd.extend_from_slice(&(-1i32).to_be_bytes()); // modifier
                    rd.extend_from_slice(&0i16.to_be_bytes()); // text format
                    out.extend(msg(b'T', &rd));
                    let mut dr = 1i16.to_be_bytes().to_vec();
                    dr.extend_from_slice(&1i32.to_be_bytes());
                    dr.push(b'1');
                    out.extend(msg(b'D', &dr));
                    out.extend(msg(b'C', &cstr("SELECT 1")));
                } else {
                    let first = q.split_whitespace().next().unwrap_or("OK").to_string();
                    out.extend(msg(b'C', &cstr(&first)));
                }
                out.extend(msg(b'Z', b"I"));
                if sock.write_all(&out).await.is_err() {
                    return;
                }
            }
            // Anything else (extended protocol) is outside this stand-in.
            _ => return,
        }
    }
}

async fn serve_auth_error(mut sock: TcpStream) {
    if !read_startup(&mut sock).await {
        return;
    }
    let mut body = Vec::new();
    for (field, value) in [
        (b'S', "FATAL"),
        (b'V', "FATAL"),
        (b'C', "28P01"),
        (b'M', "password authentication failed for user \"fake\""),
    ] {
        body.push(field);
        body.extend(cstr(value));
    }
    body.push(0);
    let _ = sock.write_all(&msg(b'E', &body)).await;
    let _ = sock.shutdown().await;
}
