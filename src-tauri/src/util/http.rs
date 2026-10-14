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
//! # Reach — what "every client" does and does not cover
//!
//! It covers every FIRST-PARTY client: everything `src-tauri/src/**` builds,
//! machine-checked by `scripts/check_reqwest_client_construction.py`. It does
//! NOT reach a client a third-party crate builds for itself out of the same
//! `reqwest 0.13`, and removing the `hickory-dns` feature removed it from the
//! whole dependency graph, so those crates now get `GaiResolver` on tokio's
//! blocking pool — partially re-exposing them to #918's starvation flood.
//! Measured on this graph:
//!
//! * `opentelemetry-otlp` / `opentelemetry-http` — a process-lifetime periodic
//!   exporter, exactly the class this plan is about. Its builder takes
//!   `.with_http_client(...)`, so `src-tauri/src/otel.rs` injects a client from
//!   [`client_builder`]; it is on the same reqwest 0.13, so the injection is
//!   type-compatible.
//! * `tauri`, `tauri-plugin-updater`, `postgresql_archive` — **no injection
//!   point.** They are not routed here, and no claim to the contrary is made
//!   anywhere in this tree.
//! * `sentry` rides a SEPARATE `reqwest 0.12`, which never had `hickory-dns`,
//!   so nothing about it changed either way.
//!
//! # Public API
//!
//! * [`client_builder`] / [`blocking_client_builder`] — start every client here.
//! * [`ResettableClient`] — a long-lived client that rebuilds itself after
//!   consecutive transport failures.
//! * [`classify_transport`] — which faults a rebuild can actually fix.
//! * [`dns_lane_class`] — the lane class hiding inside a `reqwest::Error`.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Default connect-phase ceiling (DNS + TCP + TLS). A black-holed route fails
/// in this long rather than the whole-request timeout. Callers that already set
/// their own `connect_timeout` simply override it.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Overrides [`DEFAULT_CONNECT_TIMEOUT`], in milliseconds. `0` and unparseable
/// are ignored.
///
/// The lever exists because this ceiling and the DNS lane's deadline together
/// SHRANK the effective budget for coord: `coord_http`'s client carried no
/// `connect_timeout` at all before this plan, and hickory allowed several
/// attempts. A box whose cold `getaddrinfo` takes 5–6s would otherwise fail
/// every first request with no dial to turn — and a running runner cannot be
/// restarted to change a constant (served policy `production-and-cost`
/// `runner-lifecycle`).
pub const ENV_CONNECT_TIMEOUT_MS: &str = "QONTINUI_HTTP_CONNECT_TIMEOUT_MS";

/// The connect-phase ceiling in force: [`ENV_CONNECT_TIMEOUT_MS`] if it parses
/// to a positive integer, else [`DEFAULT_CONNECT_TIMEOUT`]. Reported in every
/// [`crate::util::dns::DnsLaneSnapshot`] so an envelope names the value that
/// was actually applied.
pub fn connect_timeout() -> Duration {
    connect_timeout_from(std::env::var(ENV_CONNECT_TIMEOUT_MS).ok().as_deref())
}

/// The parsing half, kept pure: a test that set the real variable would change
/// the connect timeout of every client built by every other test running in
/// parallel.
fn connect_timeout_from(raw: Option<&str>) -> Duration {
    match raw.map(|s| s.trim().parse::<u64>()) {
        Some(Ok(ms)) if ms > 0 => Duration::from_millis(ms),
        _ => DEFAULT_CONNECT_TIMEOUT,
    }
}

/// Consecutive transport failures before a [`ResettableClient`] rebuilds.
pub const DEFAULT_REBUILD_THRESHOLD: u32 = 3;

/// Minimum spacing between two rebuilds of one [`ResettableClient`].
pub const DEFAULT_MIN_REBUILD_INTERVAL: Duration = Duration::from_secs(60);

/// An async client builder resolving through the shared OS DNS lane, with
/// [`connect_timeout`]. Add per-client settings, then `.build()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    // reqwest-client-ok: the sanctioned factory.
    reqwest::Client::builder()
        .dns_resolver(crate::util::dns::shared())
        .connect_timeout(connect_timeout())
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
        .connect_timeout(connect_timeout())
}

/// The factory's `reqwest::Client::new()`: [`client_builder`] with no further
/// settings.
///
/// # Panics
///
/// Exactly when `reqwest::Client::new()` does — if the TLS backend cannot be
/// initialised or the system configuration cannot be loaded. Call sites that
/// used `Client::new()` accepted that panic; one that must not panic should
/// use `client_builder().build()` and handle the error.
pub fn new_client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("util::http::new_client: reqwest client construction failed (TLS backend init)")
}

/// The `reqwest::blocking` twin of [`new_client`].
///
/// # Panics
///
/// Exactly when `reqwest::blocking::Client::new()` does (TLS backend init,
/// or when called from inside an async runtime's execution context).
pub fn new_blocking_client() -> reqwest::blocking::Client {
    blocking_client_builder().build().expect(
        "util::http::new_blocking_client: reqwest client construction failed (TLS backend init)",
    )
}

/// What a failed request's transport fault was, for the one decision that
/// depends on it: could rebuilding this client possibly fix it?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportFault {
    /// Connect / TCP / TLS, or a DNS answer of "no such host". A rebuild drops
    /// a pool that may hold connections bound to a vanished local address, so
    /// it can help.
    Rebuildable,
    /// The shared DNS lane refused, was unavailable, or timed out (the carried
    /// value is [`crate::util::dns::DnsLaneError::class`]).
    ///
    /// **A rebuild cannot fix this and must not be attempted.** The lane is
    /// process-wide: a rebuilt client installs the SAME lane, so the rebuild is
    /// a guaranteed no-op — and because a rebuild resets the streak, counting
    /// it would make `client_health.consecutive_transport_failures` understate
    /// the outage it was added to diagnose, while the rebuild WARN asserted a
    /// heal that did not happen.
    DnsLane(&'static str),
    /// Not a transport fault: a decode, a redirect policy, a 5xx body — or a
    /// legitimate whole-request timeout. `COORD_PROXY_CLIENT` allows 300s
    /// precisely because coord tool calls run long, and such a timeout has
    /// `is_timeout()` true with `is_connect()` false; blaming the transport for
    /// three slow-but-successful calls would rebuild a healthy client.
    None,
}

/// The [`crate::util::dns::DnsLaneError`] class hiding in a `reqwest::Error`'s
/// source chain, if any. reqwest boxes a resolver error, so the typed class
/// survives — it just has to be dug out rather than inferred from
/// `is_connect()`, which is true for every one of them.
pub fn dns_lane_class(e: &reqwest::Error) -> Option<&'static str> {
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = src {
        if let Some(lane) = s.downcast_ref::<crate::util::dns::DnsLaneError>() {
            return Some(lane.class());
        }
        src = s.source();
    }
    None
}

/// Classify one failed request. See [`TransportFault`].
pub fn classify_transport(e: &reqwest::Error) -> TransportFault {
    if let Some(class) = dns_lane_class(e) {
        return match class {
            // The lane itself failed to run the lookup.
            "saturated" | "unavailable" | "timeout" => TransportFault::DnsLane(class),
            // `lookup_failed`: the OS resolver ANSWERED (with an error). That
            // is a real resolution outcome, not a lane fault.
            _ => TransportFault::Rebuildable,
        };
    }
    // `is_connect()` unconditionally — it covers a connect-phase timeout, for
    // which reqwest reports both `is_connect()` and `is_timeout()`. A timeout
    // WITHOUT `is_connect()` is the caller's own request deadline.
    if e.is_connect() {
        return TransportFault::Rebuildable;
    }
    TransportFault::None
}

/// Whether this error is a transport fault of any kind — a rebuildable one or
/// a lane fault. Used to decide whether the DNS-vs-TCP discriminator has
/// anything to say, NOT whether to rebuild (see [`classify_transport`]).
pub fn is_transport_failure(e: &reqwest::Error) -> bool {
    classify_transport(e) != TransportFault::None
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
    /// Faults a rebuild cannot fix, counted here instead of being folded into
    /// the rebuild streak — otherwise a DNS-lane outage would be invisible in
    /// the envelope exactly when it mattered. See [`TransportFault::DnsLane`].
    dns_lane_failures: AtomicU64,
    last_dns_lane_class: Mutex<Option<&'static str>>,
    /// Failures reading a response BODY after headers had already arrived. The
    /// connect phase demonstrably worked, so this is not a connect fault and
    /// never drives a rebuild; it is the stale-pooled-connection signal.
    body_failures: AtomicU64,
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
            dns_lane_failures: AtomicU64::new(0),
            last_dns_lane_class: Mutex::new(None),
            body_failures: AtomicU64::new(0),
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
    /// redirect policy, a whole-request timeout) neither count nor reset the
    /// streak; a DNS-lane fault is counted separately because a rebuild cannot
    /// fix it. Returns `true` iff this call rebuilt the client.
    pub fn record_failure(&self, e: &reqwest::Error) -> bool {
        match classify_transport(e) {
            TransportFault::Rebuildable => self.note_transport_failure(),
            TransportFault::DnsLane(class) => {
                self.note_dns_lane_failure(class);
                false
            }
            TransportFault::None => false,
        }
    }

    /// Report a failure reading a response BODY (headers had already arrived).
    /// Counted for diagnosis; never toward a rebuild.
    pub fn record_body_failure(&self, _e: &reqwest::Error) {
        self.body_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Consecutive REBUILDABLE transport failures since the last success or
    /// rebuild. A DNS-lane outage does not move this — read
    /// [`dns_lane_failures`](Self::dns_lane_failures) for that.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Rebuilds performed since construction.
    pub fn rebuilds(&self) -> u64 {
        self.rebuilds.load(Ordering::Relaxed)
    }

    /// Failures attributed to the shared DNS lane since construction.
    pub fn dns_lane_failures(&self) -> u64 {
        self.dns_lane_failures.load(Ordering::Relaxed)
    }

    /// Response-body read failures since construction.
    pub fn body_failures(&self) -> u64 {
        self.body_failures.load(Ordering::Relaxed)
    }

    /// The per-client health block carried by an egress envelope.
    ///
    /// `note` is not decoration: without it a reader who sees
    /// `consecutive_transport_failures: 0` during a DNS outage would conclude
    /// the transport was fine, which is the misreading this split created and
    /// must therefore explain in the same object.
    pub fn health_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "consecutive_transport_failures": self.consecutive_failures(),
            "rebuilds": self.rebuilds(),
            "dns_lane_failures": self.dns_lane_failures(),
            "last_dns_lane_class": *self
                .last_dns_lane_class
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
            "body_read_failures": self.body_failures(),
            "note": "dns_lane_failures and body_read_failures are deliberately NOT in \
                     consecutive_transport_failures: the dns lane is process-wide (a rebuilt \
                     client installs the same lane) and a body read failure proves the connect \
                     phase worked, so a rebuild fixes neither",
        })
    }

    fn note_dns_lane_failure(&self, class: &'static str) {
        let n = self.dns_lane_failures.fetch_add(1, Ordering::Relaxed) + 1;
        *self
            .last_dns_lane_class
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(class);
        tracing::warn!(
            "egress: {} saw a dns-lane {class} failure (#{n}); NOT rebuilding — the lane is \
             process-wide, so a fresh client would install the same one",
            self.name
        );
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

    // -----------------------------------------------------------------
    // F2 / F3: only rebuild for faults a rebuild can fix
    // -----------------------------------------------------------------

    /// A lane whose worker threads can never be started, so every lookup
    /// through it ends as `DnsLaneError::Unavailable`.
    fn dead_lane(prefix: &'static str) -> Arc<OsDnsResolver> {
        let spawn: crate::util::dns::SpawnFn =
            Arc::new(|_n, _b| Err(std::io::Error::other("spawn refused (test)")));
        Arc::new(OsDnsResolver::with_lookup_and_spawn(
            DnsLaneConfig {
                threads: 1,
                queue_capacity: 4,
                deadline: Duration::from_millis(200),
                thread_name_prefix: prefix.to_string(),
            },
            Arc::new(|_h: &str| Ok(vec![SocketAddr::from(([10, 0, 0, 1], 0))])),
            spawn,
        ))
    }

    /// The whole of F2: a lane fault is real, is reported, and must NOT rebuild
    /// the client — a rebuilt client installs the same process-wide lane, and
    /// the rebuild would reset the streak the envelope reports.
    #[tokio::test]
    async fn a_dns_lane_fault_is_counted_and_never_rebuilds() {
        let lane = dead_lane("dns-lane-t-http-dead");
        let built = Arc::new(AtomicU32::new(0));
        let built_f = Arc::clone(&built);
        let lane_f = Arc::clone(&lane);
        // Threshold 1: if lane faults counted at all, the FIRST one would
        // rebuild, so this test cannot pass by accident.
        let c = ResettableClient::with_policy("t-lane", 1, Duration::ZERO, move || {
            built_f.fetch_add(1, Ordering::Relaxed);
            // reqwest-client-ok: test client on an injected dead lane.
            reqwest::Client::builder()
                .dns_resolver(Arc::clone(&lane_f))
                .connect_timeout(Duration::from_secs(2))
                .no_proxy()
                .build()
        })
        .unwrap();

        for i in 0..3 {
            let r = c.client().get("http://lane.test/").send().await;
            let err = r.as_ref().expect_err("the lane cannot resolve anything");
            assert_eq!(
                dns_lane_class(err),
                Some("unavailable"),
                "the typed lane class must survive reqwest's wrapping: {err:?}"
            );
            assert_eq!(
                classify_transport(err),
                TransportFault::DnsLane("unavailable")
            );
            assert!(is_transport_failure(err), "it IS a transport fault");
            assert!(!c.record_outcome(&r), "iteration {i} must not rebuild");
        }
        assert_eq!(c.rebuilds(), 0);
        assert_eq!(
            built.load(Ordering::Relaxed),
            1,
            "built once, never rebuilt"
        );
        assert_eq!(
            c.consecutive_failures(),
            0,
            "the rebuild streak must stay clean so it keeps meaning what it says"
        );
        assert_eq!(c.dns_lane_failures(), 3, "the outage IS reported");
        let h = c.health_json();
        assert_eq!(h["dns_lane_failures"], 3, "{h}");
        assert_eq!(h["last_dns_lane_class"], "unavailable", "{h}");
        assert!(
            h["note"].as_str().unwrap_or_default().contains("dns lane"),
            "the envelope must explain the zero streak: {h}"
        );
    }

    /// A server that accepts and then says nothing: the client's OWN
    /// whole-request timeout fires, with the transport in perfect health.
    fn serve_stalling() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            while let Ok((s, _)) = l.accept() {
                std::thread::sleep(Duration::from_millis(1500));
                drop(s);
            }
        });
        addr
    }

    /// F3: `COORD_PROXY_CLIENT` allows 300s because coord tool calls run long.
    /// Three slow-but-successful-transport calls must not rebuild it.
    #[tokio::test]
    async fn a_whole_request_timeout_is_not_a_transport_failure() {
        let stalling = serve_stalling();
        let built = Arc::new(AtomicU32::new(0));
        let built_f = Arc::clone(&built);
        let c = ResettableClient::with_policy("t-slow", 1, Duration::ZERO, move || {
            built_f.fetch_add(1, Ordering::Relaxed);
            client_builder()
                .timeout(Duration::from_millis(250))
                .no_proxy()
                .build()
        })
        .unwrap();
        let url = format!("http://{stalling}/");
        for _ in 0..3 {
            let r = c.client().get(&url).send().await;
            let err = r.as_ref().expect_err("the request times out");
            assert!(err.is_timeout(), "{err:?}");
            assert!(
                !err.is_connect(),
                "the connect phase SUCCEEDED — the server accepted: {err:?}"
            );
            assert_eq!(classify_transport(err), TransportFault::None, "{err:?}");
            assert!(!c.record_outcome(&r), "a long request must not rebuild");
        }
        assert_eq!(c.rebuilds(), 0);
        assert_eq!(c.consecutive_failures(), 0);
        assert_eq!(built.load(Ordering::Relaxed), 1);
    }

    /// A connect-phase failure still counts: this is the fault a rebuild can
    /// actually fix, and F3 must not have disarmed it.
    #[tokio::test]
    async fn a_connect_failure_is_still_rebuildable() {
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let c = ResettableClient::with_policy("t-connect", 2, Duration::ZERO, || {
            client_builder()
                .connect_timeout(Duration::from_millis(300))
                .no_proxy()
                .build()
        })
        .unwrap();
        let url = format!("http://{dead}/");
        let r1 = c.client().get(&url).send().await;
        assert_eq!(
            classify_transport(r1.as_ref().unwrap_err()),
            TransportFault::Rebuildable
        );
        assert!(!c.record_outcome(&r1));
        let r2 = c.client().get(&url).send().await;
        assert!(c.record_outcome(&r2), "the second connect failure rebuilds");
        assert_eq!(c.rebuilds(), 1);
        assert_eq!(c.dns_lane_failures(), 0);
    }

    /// A body-read failure is counted apart from the rebuild streak: headers
    /// had arrived, so the connect phase demonstrably worked.
    #[tokio::test]
    async fn a_body_failure_is_counted_apart_from_the_rebuild_streak() {
        let stalling = serve_stalling();
        let c = ResettableClient::with_policy("t-body", 1, Duration::ZERO, || {
            client_builder()
                .timeout(Duration::from_millis(250))
                .no_proxy()
                .build()
        })
        .unwrap();
        let err = c
            .client()
            .get(format!("http://{stalling}/"))
            .send()
            .await
            .expect_err("times out");
        c.record_body_failure(&err);
        assert_eq!(c.body_failures(), 1);
        assert_eq!(c.rebuilds(), 0);
        assert_eq!(c.consecutive_failures(), 0);
        assert_eq!(c.health_json()["body_read_failures"], 1);
    }

    #[test]
    fn the_connect_timeout_dial_parses_or_falls_back() {
        assert_eq!(connect_timeout_from(None), DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(
            connect_timeout_from(Some(" 2500 ")),
            Duration::from_millis(2500)
        );
        assert_eq!(connect_timeout_from(Some("0")), DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(connect_timeout_from(Some("soon")), DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(connect_timeout_from(Some("")), DEFAULT_CONNECT_TIMEOUT);
    }

    #[test]
    fn builders_construct() {
        client_builder().build().expect("async client");
        // The blocking client spins its own runtime thread; build it off any
        // async context (this is a plain #[test]).
        blocking_client_builder().build().expect("blocking client");
    }
}
