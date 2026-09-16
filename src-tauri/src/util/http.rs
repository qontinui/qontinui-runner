//! THE place a runner `reqwest` client is constructed.
//!
//! Plan `2026-09-16-runner-long-lived-http-clients-freeze-dns-state-across-network-changes`,
//! Phases 3 (builder) and 4 (self-healing backstop).
//!
//! # Why a factory, and why here rather than `coord_http.rs`
//!
//! Every client must resolve through [`crate::util::dns::shared`] — the OS
//! resolver on a dedicated bounded lane. A client built any other way silently
//! gets either reqwest's per-client hickory resolver (frozen at first lookup:
//! the 2026-09-16 90-minute coord outage) or, once the `hickory-dns` feature is
//! removed, `GaiResolver` on tokio's blocking pool (#918's starvation flood).
//! `scripts/check_reqwest_client_construction.py` fails CI on a construction
//! outside this file.
//!
//! `coord_http.rs` is the home of the COORD client and its request helpers; it
//! is bin-only and coord-specific. The builder applies to every HTTP client in
//! the process (LLM providers, the embedding service, UI Bridge fetches) and
//! must be reachable from the lib crate too (`auth`, `pair`, `cognito` build
//! clients there), so it lives in `util`, declared in BOTH crates' `util`
//! modules — the bin re-exports the lib's copy so there is one DNS lane per
//! process, not one per crate.
//!
//! # Public API
//!
//! * [`client_builder`] / [`blocking_client_builder`] — start every client here.
//! * [`ResettableClient`] — a long-lived client that rebuilds itself after
//!   consecutive transport failures.
//! * [`is_transport_failure`] — the classifier it uses.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Default connect-phase ceiling (DNS + TCP + TLS). A black-holed route fails
/// in this long rather than the whole-request timeout. Callers that already set
/// their own `connect_timeout` simply override it.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Consecutive transport failures before a [`ResettableClient`] rebuilds.
pub const DEFAULT_REBUILD_THRESHOLD: u32 = 3;

/// Minimum spacing between two rebuilds of one [`ResettableClient`].
pub const DEFAULT_MIN_REBUILD_INTERVAL: Duration = Duration::from_secs(60);

/// An async client builder resolving through the shared OS DNS lane, with
/// [`DEFAULT_CONNECT_TIMEOUT`]. Add per-client settings, then `.build()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    // reqwest-client-ok: the sanctioned factory.
    reqwest::Client::builder()
        .dns_resolver(crate::util::dns::shared())
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
}

/// The `reqwest::blocking` twin of [`client_builder`].
///
/// The blocking builder supports `dns_resolver` in reqwest 0.13, so the same
/// shared lane applies. As with any blocking client, never `build()` or use it
/// from inside an async context.
pub fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    // reqwest-client-ok: the sanctioned factory.
    reqwest::blocking::Client::builder()
        .dns_resolver(crate::util::dns::shared())
        .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
}

/// A failure of the transport itself — connect (which includes DNS) or a
/// timeout — as opposed to coord answering badly. Only these count toward a
/// rebuild: a 5xx proves the transport works.
pub fn is_transport_failure(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout()
}

type Factory = Box<dyn Fn() -> Result<reqwest::Client, reqwest::Error> + Send + Sync>;

/// A long-lived `reqwest::Client` that rebuilds itself after repeated transport
/// failures.
///
/// Even with OS-backed DNS a process-lifetime pool can hold state across a
/// network change (connections bound to a vanished local address, a pool of
/// half-dead keep-alives). Nothing else in the runner reacts to consecutive
/// egress failures, and restarting the runner is forbidden to agents, so an
/// unhealed client is an unbounded outage. After `threshold` consecutive
/// transport failures the client is rebuilt from its factory — at most once per
/// `min_rebuild_interval` — and the rebuild is logged:
/// `egress: rebuilt <name> after N consecutive transport failures`.
///
/// Requests already in flight keep the client they cloned; only new requests
/// see the rebuilt one.
///
/// Usage: take [`client`](Self::client) per request, and report the send's
/// result with [`record_outcome`](Self::record_outcome).
pub struct ResettableClient {
    name: &'static str,
    current: RwLock<reqwest::Client>,
    factory: Factory,
    threshold: u32,
    min_rebuild_interval: Duration,
    consecutive_failures: AtomicU32,
    rebuilds: AtomicU64,
    last_rebuild: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for ResettableClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResettableClient")
            .field("name", &self.name)
            .field("consecutive_failures", &self.consecutive_failures())
            .field("rebuilds", &self.rebuilds())
            .finish()
    }
}

impl ResettableClient {
    /// Build the first client from `factory` with the default policy
    /// ([`DEFAULT_REBUILD_THRESHOLD`], [`DEFAULT_MIN_REBUILD_INTERVAL`]).
    /// The factory should start from [`client_builder`].
    pub fn new<F>(name: &'static str, factory: F) -> Result<Self, reqwest::Error>
    where
        F: Fn() -> Result<reqwest::Client, reqwest::Error> + Send + Sync + 'static,
    {
        Self::with_policy(
            name,
            DEFAULT_REBUILD_THRESHOLD,
            DEFAULT_MIN_REBUILD_INTERVAL,
            factory,
        )
    }

    /// As [`new`](Self::new) with an explicit rebuild policy.
    pub fn with_policy<F>(
        name: &'static str,
        threshold: u32,
        min_rebuild_interval: Duration,
        factory: F,
    ) -> Result<Self, reqwest::Error>
    where
        F: Fn() -> Result<reqwest::Client, reqwest::Error> + Send + Sync + 'static,
    {
        let first = factory()?;
        Ok(Self {
            name,
            current: RwLock::new(first),
            factory: Box::new(factory),
            threshold: threshold.max(1),
            min_rebuild_interval,
            consecutive_failures: AtomicU32::new(0),
            rebuilds: AtomicU64::new(0),
            last_rebuild: Mutex::new(None),
        })
    }

    /// The stable name used in the rebuild log line.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The current client (a cheap `Arc` clone sharing its pool).
    pub fn client(&self) -> reqwest::Client {
        self.current
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Report one request's `send()` result. Returns `true` iff this call
    /// rebuilt the client.
    pub fn record_outcome<T>(&self, outcome: &Result<T, reqwest::Error>) -> bool {
        match outcome {
            Ok(_) => {
                self.record_success();
                false
            }
            Err(e) => self.record_failure(e),
        }
    }

    /// The transport worked (any HTTP status). Resets the failure streak.
    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    /// Report a failed request. Non-transport errors (a body decode, a
    /// redirect policy) neither count nor reset the streak. Returns `true` iff
    /// this call rebuilt the client.
    pub fn record_failure(&self, e: &reqwest::Error) -> bool {
        if !is_transport_failure(e) {
            return false;
        }
        self.note_transport_failure()
    }

    /// Consecutive transport failures since the last success or rebuild.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Rebuilds performed since construction.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds.load(Ordering::Relaxed)
    }

    /// `{name, consecutive_transport_failures, rebuilds}` for egress envelopes.
    pub fn health_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "consecutive_transport_failures": self.consecutive_failures(),
            "rebuilds": self.rebuilds(),
        })
    }

    fn note_transport_failure(&self) -> bool {
        let n = self
            .consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if n < self.threshold {
            return false;
        }
        let mut last = self.last_rebuild.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(at) = *last {
            if at.elapsed() < self.min_rebuild_interval {
                tracing::debug!(
                    "egress: {} has {n} consecutive transport failures; rebuild rate-limited \
                     (last rebuild {:?} ago)",
                    self.name,
                    at.elapsed()
                );
                return false;
            }
        }
        // A racing failure may already have rebuilt (and reset the streak)
        // while this one waited for the lock.
        let n = self.consecutive_failures.load(Ordering::Relaxed);
        if n < self.threshold {
            return false;
        }
        match (self.factory)() {
            Ok(fresh) => {
                *self.current.write().unwrap_or_else(|p| p.into_inner()) = fresh;
                *last = Some(Instant::now());
                self.consecutive_failures.store(0, Ordering::Relaxed);
                let total = self.rebuilds.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(
                    "egress: rebuilt {} after {n} consecutive transport failures (rebuild #{total})",
                    self.name
                );
                true
            }
            Err(e) => {
                // Keep the old client; stamp the attempt so a broken factory is
                // not retried on every failure.
                *last = Some(Instant::now());
                tracing::warn!(
                    "egress: rebuilding {} after {n} consecutive transport failures failed ({e}); \
                     keeping the previous client",
                    self.name
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::dns::{DnsLaneConfig, LookupFn, OsDnsResolver};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::Arc;

    fn counting_factory() -> (
        Arc<AtomicU32>,
        impl Fn() -> Result<reqwest::Client, reqwest::Error>,
    ) {
        let built = Arc::new(AtomicU32::new(0));
        let b = Arc::clone(&built);
        (built, move || {
            b.fetch_add(1, Ordering::Relaxed);
            client_builder().build()
        })
    }

    #[test]
    fn rebuilds_after_threshold_not_before() {
        let (built, factory) = counting_factory();
        let c = ResettableClient::with_policy("t-threshold", 3, Duration::ZERO, factory).unwrap();
        assert_eq!(built.load(Ordering::Relaxed), 1);
        assert!(!c.note_transport_failure());
        assert!(!c.note_transport_failure());
        assert_eq!(c.rebuilds(), 0);
        assert!(
            c.note_transport_failure(),
            "third consecutive failure rebuilds"
        );
        assert_eq!(c.rebuilds(), 1);
        assert_eq!(built.load(Ordering::Relaxed), 2);
        assert_eq!(c.consecutive_failures(), 0, "streak resets on rebuild");
    }

    #[test]
    fn success_resets_the_streak() {
        let (_built, factory) = counting_factory();
        let c = ResettableClient::with_policy("t-reset", 3, Duration::ZERO, factory).unwrap();
        c.note_transport_failure();
        c.note_transport_failure();
        c.record_success();
        assert!(!c.note_transport_failure());
        assert!(!c.note_transport_failure());
        assert_eq!(c.rebuilds(), 0);
        assert!(c.note_transport_failure());
    }

    #[test]
    fn rebuild_is_rate_limited() {
        let (built, factory) = counting_factory();
        let interval = Duration::from_millis(300);
        let c = ResettableClient::with_policy("t-rate", 1, interval, factory).unwrap();
        assert!(c.note_transport_failure(), "first rebuild is allowed");
        assert!(
            !c.note_transport_failure(),
            "second inside the interval is refused"
        );
        assert!(!c.note_transport_failure());
        assert_eq!(c.rebuilds(), 1);
        assert_eq!(built.load(Ordering::Relaxed), 2);
        std::thread::sleep(interval + Duration::from_millis(50));
        assert!(
            c.note_transport_failure(),
            "allowed again after the interval"
        );
        assert_eq!(c.rebuilds(), 2);
    }

    /// Driven by REAL reqwest connect failures against a refusing port, then a
    /// real success against a listener that accepts.
    #[tokio::test]
    async fn real_connect_failures_trigger_rebuild_and_success_resets() {
        // A port nothing listens on: bind, read the port, close.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let built = Arc::new(AtomicU32::new(0));
        let built_f = Arc::clone(&built);
        let c = ResettableClient::with_policy("t-real", 2, Duration::ZERO, move || {
            built_f.fetch_add(1, Ordering::Relaxed);
            // Short connect timeout: Windows retries a refused SYN for ~2s.
            client_builder()
                .connect_timeout(Duration::from_millis(300))
                .no_proxy()
                .build()
        })
        .unwrap();
        let url = format!("http://{dead}/");
        let r1 = c.client().get(&url).send().await;
        assert!(
            r1.as_ref().err().is_some_and(is_transport_failure),
            "{r1:?}"
        );
        assert!(!c.record_outcome(&r1));
        let r2 = c.client().get(&url).send().await;
        assert!(
            c.record_outcome(&r2),
            "second transport failure rebuilds at threshold 2"
        );
        assert_eq!(c.rebuilds(), 1);
        assert_eq!(built.load(Ordering::Relaxed), 2);

        let r3 = c.client().get(&url).send().await;
        assert!(!c.record_outcome(&r3));
        assert_eq!(c.consecutive_failures(), 1);
        let live = serve_n("L", 1);
        let ok = c.client().get(format!("http://{live}/")).send().await;
        assert!(ok.is_ok(), "{ok:?}");
        assert!(!c.record_outcome(&ok));
        assert_eq!(c.consecutive_failures(), 0, "a success resets the streak");
        assert_eq!(c.rebuilds(), 1);
    }

    /// A one-response-per-connection HTTP server answering `body`, `n` times.
    fn serve_n(body: &'static str, n: usize) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        addr
    }

    /// Regression for the 2026-09-16 episode: a LONG-LIVED client whose
    /// resolver's backing answer changes must follow the change on its next
    /// connection, with no rebuild. reqwest's per-client hickory resolver
    /// violates exactly this (it snapshots the system DNS config once).
    #[tokio::test]
    async fn long_lived_client_follows_a_changing_os_answer_without_rebuild() {
        let a = serve_n("A", 1);
        let b = serve_n("B", 1);
        let answer = Arc::new(Mutex::new(a));
        let answer_c = Arc::clone(&answer);
        // Stands in for the OS resolver: its answer is whatever the "network"
        // currently says.
        let lookup: LookupFn = Arc::new(move |host: &str| {
            assert_eq!(host, "flip.test");
            Ok(vec![*answer_c.lock().unwrap()])
        });
        let lane = Arc::new(OsDnsResolver::with_lookup(
            DnsLaneConfig {
                threads: 1,
                queue_capacity: 4,
                deadline: Duration::from_secs(2),
                thread_name_prefix: "dns-lane-t-flip".to_string(),
            },
            lookup,
        ));
        let lane_f = Arc::clone(&lane);
        let (built, _) = counting_factory();
        let built_f = Arc::clone(&built);
        let c = ResettableClient::new("t-flip", move || {
            built_f.fetch_add(1, Ordering::Relaxed);
            // reqwest-client-ok: test client on an injected lane.
            reqwest::Client::builder()
                .dns_resolver(Arc::clone(&lane_f))
                .connect_timeout(Duration::from_secs(2))
                .no_proxy()
                .build()
        })
        .unwrap();

        let first = c.client().get("http://flip.test/").send().await.unwrap();
        assert_eq!(first.text().await.unwrap(), "A");

        *answer.lock().unwrap() = b;

        let second = c.client().get("http://flip.test/").send().await.unwrap();
        assert_eq!(
            second.text().await.unwrap(),
            "B",
            "the long-lived client must follow the new answer"
        );
        assert_eq!(
            c.rebuilds(),
            0,
            "following the change must not need a rebuild"
        );
        assert_eq!(built.load(Ordering::Relaxed), 1);
        assert!(
            lane.snapshot().resolved >= 2,
            "each new connection re-resolved"
        );
    }

    #[test]
    fn builders_construct() {
        client_builder().build().expect("async client");
        // The blocking client spins its own runtime thread; build it off any
        // async context (this is a plain #[test]).
        blocking_client_builder().build().expect("blocking client");
    }
}
