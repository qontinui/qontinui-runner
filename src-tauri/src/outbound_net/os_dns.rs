//! The runner's DNS resolver for shared `reqwest` clients: the OPERATING
//! SYSTEM's resolver (`getaddrinfo`), run on a small bounded set of dedicated
//! threads rather than on tokio's blocking pool.
//!
//! # Why not hickory
//!
//! The runner briefly built every `reqwest::Client` with the `hickory-dns`
//! feature. Hickory does not ask the OS to resolve a name; it reads the list
//! of nameservers configured on every network adapter
//! (`hickory_resolver::system_conf::read_system_conf`) and queries those
//! servers itself, with a 5 s per-query timeout. That list is not what the OS
//! actually uses. Windows routes a query by interface metric, NRPT rules and a
//! VPN's DNS leak protection; hickory sees none of that.
//!
//! Measured on 2026-10-18 on a box running a WireGuard VPN (ProtonVPN), whose
//! leak protection drops DNS to the physical adapter's ISP servers: hickory's
//! system config listed the VPN's server and both blocked ISP servers. Every
//! FRESH hickory resolver took 5.5 s to resolve `coord.qontinui.io` (one
//! blocked server's 5 s timeout, then an answer), against 0.37 s through
//! `getaddrinfo`. reqwest builds a fresh resolver per `Client`, so every
//! per-request client, including the 30 s device-register heartbeat with its
//! 5 s cap, timed out before it sent a byte. The box's
//! `coord_drain_state` went UNKNOWN for 33 hours and autonomous spawns paused,
//! although coord answered the same request from the same box in 0.4 s.
//! The runner was never slow; the resolver was.
//!
//! # Why not plain `GaiResolver` on the shared client either
//!
//! reqwest's default `GaiResolver` also calls `getaddrinfo`, but it does so
//! through `tokio::task::spawn_blocking`, which takes a slot on the RUNTIME's
//! blocking pool. On the main runtime that pool is shared with every other
//! blocking task, and it has been pinned near its 512-thread ceiling before,
//! which queued DNS behind unrelated work until requests timed out. That is
//! the incident the bin's shared `coord_http::coord_client` was introduced for.
//! [`OsResolver`] keeps the OS's answer and avoids that queue by running each
//! lookup on its own short-lived thread, gated by a process-wide semaphore of
//! [`MAX_CONCURRENT_LOOKUPS`] permits. A stuck lookup can therefore hold one
//! permit, never a blocking-pool slot.
//!
//! Build clients with [`client_builder`], which installs it:
//!
//! ```ignore
//! let client = qontinui_runner_lib::outbound_net::os_dns::client_builder()
//!     .timeout(std::time::Duration::from_secs(5))
//!     .build()?;
//! ```

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::sync::Semaphore;

/// The most `getaddrinfo` calls this process runs at once through
/// [`OsResolver`]. A lookup over the limit waits asynchronously for a permit;
/// it does not take a thread while it waits.
pub const MAX_CONCURRENT_LOOKUPS: usize = 8;

/// The longest [`OsResolver`] waits for one name, permit wait included. A
/// Windows `getaddrinfo` against an unresponsive server can block well past
/// this; the lookup thread then finishes on its own and its answer is
/// discarded, but the REQUEST fails here with an error naming the host rather
/// than hanging on a caller that set no per-request timeout.
pub const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A [`reqwest::dns::Resolve`] that asks the operating system's resolver.
///
/// Cheap to clone; every instance shares the one process-wide permit pool.
#[derive(Clone, Debug, Default)]
pub struct OsResolver;

fn permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_LOOKUPS)))
}

/// The shared [`OsResolver`], as the `Arc` that
/// [`reqwest::ClientBuilder::dns_resolver`] takes.
pub fn os_resolver() -> Arc<OsResolver> {
    static RESOLVER: OnceLock<Arc<OsResolver>> = OnceLock::new();
    RESOLVER.get_or_init(|| Arc::new(OsResolver)).clone()
}

/// A `reqwest::ClientBuilder` that resolves through [`OsResolver`]. Prefer it
/// to a bare `reqwest::Client::builder()` for any new client: the default
/// `GaiResolver` takes a slot on the calling runtime's blocking pool per
/// lookup.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(os_resolver())
}

/// Resolve `host` through the OS resolver on a dedicated thread, holding one
/// of the [`MAX_CONCURRENT_LOOKUPS`] permits for the duration.
///
/// The returned addresses carry port 0; reqwest substitutes the URL's port.
pub async fn resolve_host(host: String) -> Result<Vec<SocketAddr>, BoxError> {
    resolve_host_with(permits().clone(), host).await
}

/// [`resolve_host`] against an explicit permit pool, so a test can observe
/// the pool without racing every other lookup in the process.
async fn resolve_host_with(
    permits: Arc<Semaphore>,
    host: String,
) -> Result<Vec<SocketAddr>, BoxError> {
    let permit = permits
        .acquire_owned()
        .await
        .map_err(|e| -> BoxError { format!("dns permit pool closed: {e}").into() })?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let thread_host = host.clone();
    std::thread::Builder::new()
        .name("runner-dns".to_string())
        .spawn(move || {
            // `(host, 0).to_socket_addrs()` is `getaddrinfo`. The permit is
            // released only when the OS call returns, so a hung lookup is
            // counted against the bound for as long as it actually runs.
            let result = (thread_host.as_str(), 0u16)
                .to_socket_addrs()
                .map(|it| it.collect::<Vec<_>>());
            drop(permit);
            let _ = tx.send(result);
        })
        .map_err(|e| -> BoxError { format!("spawn dns thread for {host}: {e}").into() })?;
    let addrs = rx
        .await
        .map_err(|_| -> BoxError { format!("dns thread for {host} ended without an answer").into() })?
        .map_err(|e| -> BoxError { format!("resolve {host}: {e}").into() })?;
    if addrs.is_empty() {
        return Err(format!("resolve {host}: the OS resolver returned no addresses").into());
    }
    Ok(addrs)
}

impl Resolve for OsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::time::timeout(LOOKUP_TIMEOUT, resolve_host(host.clone()))
                .await
                .map_err(|_| -> BoxError {
                    format!("resolve {host}: no answer from the OS resolver within {LOOKUP_TIMEOUT:?}")
                        .into()
                })??;
            let it: Addrs = Box::new(addrs.into_iter());
            Ok(it)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn resolves_localhost_through_the_os() {
        let addrs = resolve_host("localhost".to_string())
            .await
            .expect("the OS resolver answers for localhost");
        assert!(addrs.iter().all(|a| a.ip().is_loopback()), "{addrs:?}");
        assert!(addrs.iter().all(|a| a.port() == 0), "{addrs:?}");
    }

    #[tokio::test]
    async fn an_unresolvable_name_is_an_error_not_an_empty_answer() {
        // `.invalid` is reserved (RFC 6761) and never resolves.
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            resolve_host("no-such-host.invalid".to_string()),
        )
        .await
        .expect("the OS resolver answers within 30 s");
        match result {
            // The normal case: the error names the host it failed on.
            Err(err) => assert!(err.to_string().contains("no-such-host.invalid"), "{err}"),
            // An NXDOMAIN-rewriting resolver (some ISPs) answers with a
            // search page's address. That is the network's answer, not ours —
            // but it must never be a loopback address or an empty list.
            Ok(addrs) => assert!(
                !addrs.is_empty() && addrs.iter().all(|a| !a.ip().is_loopback()),
                "{addrs:?}"
            ),
        }
    }

    #[tokio::test]
    async fn every_permit_is_returned_after_lookups_complete() {
        let pool = Arc::new(Semaphore::new(2));
        let lookups: Vec<_> = (0..6)
            .map(|_| tokio::spawn(resolve_host_with(pool.clone(), "localhost".to_string())))
            .collect();
        for l in lookups {
            l.await.expect("join").expect("localhost resolves");
        }
        // Each lookup thread drops its permit BEFORE it sends the answer, so
        // every permit is back once every answer has been received.
        assert_eq!(pool.available_permits(), 2);
    }

    /// The lookup must not depend on the calling runtime's blocking pool: a
    /// current-thread runtime whose blocking pool is capped at ONE thread, and
    /// that one thread parked, still resolves.
    #[test]
    fn resolves_while_the_runtime_blocking_pool_is_exhausted() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let parked = tokio::task::spawn_blocking(move || {
                let _ = release_rx.recv();
            });
            let addrs = tokio::time::timeout(
                Duration::from_secs(10),
                resolve_host("localhost".to_string()),
            )
            .await
            .expect("resolution must not queue behind the parked blocking task")
            .expect("localhost resolves");
            assert!(!addrs.is_empty());
            release_tx.send(()).expect("release the parked task");
            parked.await.expect("join the parked task");
        });
    }

    /// The root-cause pin: no `reqwest` in the lockfile may depend on
    /// `hickory-resolver`. A cargo feature is crate-global, so `hickory-dns`
    /// switched on from ANY manifest — this crate's `reqwest` line, a
    /// `[dependencies.reqwest]` table, a `[features]` entry, or another
    /// dependency via feature unification — makes every `reqwest::Client` in
    /// the process skip the OS resolver (module doc). The lockfile records the
    /// unified result, so it is the one place every route shows up.
    #[test]
    fn no_reqwest_in_the_lockfile_depends_on_hickory() {
        let lock = include_str!("../../../Cargo.lock");
        // Line-based, so a CRLF checkout of the lockfile matches too.
        let is_reqwest = |block: &str| block.lines().any(|l| l.trim_end() == "name = \"reqwest\"");
        let reqwest_blocks: Vec<&str> = lock.split("[[package]]").filter(|b| is_reqwest(b)).collect();
        // Guard the guard: finding no reqwest at all would make this vacuous.
        assert!(
            !reqwest_blocks.is_empty(),
            "Cargo.lock no longer lists reqwest where this test looks"
        );
        let offenders: Vec<&str> = reqwest_blocks
            .iter()
            .filter(|b| b.contains("\"hickory-resolver"))
            .map(|b| {
                b.lines()
                    .find(|l| l.starts_with("version = "))
                    .unwrap_or("version = ?")
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "reqwest must resolve through the OS, not hickory (see outbound_net::os_dns); \
             reqwest {offenders:?} depends on hickory-resolver in Cargo.lock"
        );
    }

    /// `reqwest` accepts the resolver and drives a real request through it.
    #[tokio::test]
    async fn a_reqwest_client_resolves_through_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(os_resolver())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");
        // `localhost` may resolve to ::1 first; the listener is IPv4, and
        // hyper falls back across the returned addresses.
        let resp = client
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .expect("request through the OS resolver");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }
}
