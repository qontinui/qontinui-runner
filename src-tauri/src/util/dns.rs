//! The process-wide DNS resolver every runner `reqwest` client resolves through:
//! the OS resolver, run on a dedicated, bounded thread lane.
//!
//! Plan `2026-09-16-runner-long-lived-http-clients-freeze-dns-state-across-network-changes`,
//! Phase 2.
//!
//! # Why this exists
//!
//! Runner #918 turned on reqwest's `hickory-dns` feature to keep DNS off tokio's
//! blocking pool (reqwest's default `GaiResolver` spends one blocking-pool slot
//! per lookup, and the runner had pinned that pool near its 512-thread ceiling).
//! The side effect: in reqwest 0.13 every `Client` owns a private
//! `OnceCell<TokioResolver>` built ONCE, on first lookup, from a snapshot of the
//! adapter DNS servers, with its own cache and name-server ranking. It is never
//! rebuilt. After a WLAN blip on 2026-09-16 the process-lifetime coord clients
//! kept failing `client error (Connect): operation timed out` for 90+ minutes
//! while curl, git and freshly-built runner clients recovered within a minute —
//! and because reqwest wraps DNS inside `connect_timeout`, a wedged resolver was
//! indistinguishable from a TCP failure.
//!
//! # What this does instead
//!
//! [`OsDnsResolver`] resolves through `std::net::ToSocketAddrs` — `getaddrinfo`,
//! i.e. the OS resolver, which tracks network changes, VPN/NRPT rules and the
//! hosts file (it is exactly what recovered for curl). The blocking call runs on
//! a FIXED set of dedicated OS threads (`dns-lane-N`) fed by a BOUNDED queue —
//! never `tokio::task::spawn_blocking`, which would reopen #918's starvation
//! flood. Every lookup carries a hard deadline, and a full queue refuses at once:
//!
//! * queue full → [`DnsLaneError::Saturated`], returned immediately;
//! * deadline passed → [`DnsLaneError::Timeout`];
//!
//! both `Display` as `dns-lane saturated` / `dns-lane timeout`, so the
//! existing `error_chain` renderer names the class in every egress WARN and 502
//! envelope instead of a bare "operation timed out".
//!
//! No positive cache is kept here: the OS already caches, and a cache in this
//! process is precisely the frozen state this module exists to remove.
//!
//! # Telemetry
//!
//! The runner's blocking-lane registry (`wedge_diagnostics::LaneTable`) counts
//! TOKIO blocking-pool bodies per spawning-thread name and is read against the
//! per-runtime 512-slot ceiling. These lookups deliberately never touch that
//! pool, so charging them there would re-create the category error that
//! module's docs describe (numbers summed over a ceiling they do not consume).
//! The lane therefore reports through its own [`OsDnsResolver::snapshot`] /
//! [`shared_snapshot`], which `util::egress_context` attaches to every egress
//! failure and to the periodic baseline.

use std::fmt;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Dedicated resolver threads in the shared lane. Lookups are short and the OS
/// caches, so a handful covers every coord host this process talks to; the
/// point of a fixed number is that a DNS outage can occupy at most this many
/// threads, never the blocking pool.
pub const DEFAULT_LANE_THREADS: usize = 4;

/// Lookups allowed to wait for a lane thread. Beyond this the lane refuses
/// immediately with [`DnsLaneError::Saturated`] — during a resolver outage a
/// growing queue only converts every request into a slower timeout.
pub const DEFAULT_QUEUE_CAPACITY: usize = 64;

/// Hard per-lookup deadline, measured from enqueue. Below the 10s
/// `connect_timeout` the runner's clients use, so a stuck lookup surfaces as a
/// typed `dns-lane timeout` rather than being swallowed by reqwest's generic
/// connect timeout.
pub const DEFAULT_LOOKUP_DEADLINE: Duration = Duration::from_secs(4);

/// A probe resolution slower than this is reported as `dns` class even when it
/// eventually succeeded.
pub const DNS_SLOW_THRESHOLD: Duration = Duration::from_millis(1000);

/// Thread-name prefix of the shared lane.
pub const SHARED_LANE_THREAD_PREFIX: &str = "dns-lane";

/// The blocking resolution a lane thread performs. Injectable so tests can
/// substitute a lookup that blocks, fails, or changes its answer.
pub type LookupFn = Arc<dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

/// The production lookup: `getaddrinfo` via `ToSocketAddrs`.
///
/// Port `0` on purpose: reqwest substitutes the URL's explicit port, or the
/// scheme default, for a resolved port of `0`.
pub fn os_lookup(host: &str) -> io::Result<Vec<SocketAddr>> {
    (host, 0u16).to_socket_addrs().map(|it| it.collect())
}

/// Shape of one lane.
#[derive(Clone, Debug)]
pub struct DnsLaneConfig {
    pub threads: usize,
    pub queue_capacity: usize,
    pub deadline: Duration,
    /// Worker threads are named `<prefix>-<index>`.
    pub thread_name_prefix: String,
}

impl Default for DnsLaneConfig {
    fn default() -> Self {
        Self {
            threads: DEFAULT_LANE_THREADS,
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            deadline: DEFAULT_LOOKUP_DEADLINE,
            thread_name_prefix: SHARED_LANE_THREAD_PREFIX.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a lane lookup produced no addresses. Every variant's `Display` begins
/// with `dns-lane <class>` so a rendered `source()` chain names the class.
#[derive(Debug)]
pub enum DnsLaneError {
    /// The bounded queue was full; refused without waiting.
    Saturated { host: String, queue_capacity: usize },
    /// No answer within the deadline (the lookup may still be running on its
    /// lane thread; its result is discarded).
    Timeout { host: String, deadline: Duration },
    /// The lane has no live worker to take the job (threads failed to spawn or
    /// died). Distinct from a timeout so it is never mistaken for a slow network.
    Unavailable { host: String },
    /// The OS resolver answered with an error (e.g. host not found).
    Lookup { host: String, source: io::Error },
}

impl DnsLaneError {
    /// Stable machine token for the error class.
    pub fn class(&self) -> &'static str {
        match self {
            DnsLaneError::Saturated { .. } => "saturated",
            DnsLaneError::Timeout { .. } => "timeout",
            DnsLaneError::Unavailable { .. } => "unavailable",
            DnsLaneError::Lookup { .. } => "lookup_failed",
        }
    }
}

impl fmt::Display for DnsLaneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DnsLaneError::Saturated {
                host,
                queue_capacity,
            } => write!(
                f,
                "dns-lane saturated: {queue_capacity} lookups already queued; refused {host} without waiting"
            ),
            DnsLaneError::Timeout { host, deadline } => write!(
                f,
                "dns-lane timeout: {host} did not resolve within {}ms",
                deadline.as_millis()
            ),
            DnsLaneError::Unavailable { host } => write!(
                f,
                "dns-lane unavailable: no live resolver thread to resolve {host}"
            ),
            DnsLaneError::Lookup { host, .. } => {
                write!(f, "dns-lane lookup failed for {host}")
            }
        }
    }
}

impl std::error::Error for DnsLaneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DnsLaneError::Lookup { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// The lane
// ---------------------------------------------------------------------------

struct Job {
    host: String,
    reply: tokio::sync::oneshot::Sender<io::Result<Vec<SocketAddr>>>,
}

#[derive(Default)]
struct Counters {
    threads_live: AtomicUsize,
    queued: AtomicUsize,
    in_flight: AtomicUsize,
    total: AtomicU64,
    resolved: AtomicU64,
    failed: AtomicU64,
    timeouts: AtomicU64,
    saturations: AtomicU64,
    /// Jobs whose caller had already given up (deadline) before a lane thread
    /// picked them up — skipped without running the lookup.
    abandoned: AtomicU64,
}

struct Inner {
    tx: SyncSender<Job>,
    counters: Arc<Counters>,
    config: DnsLaneConfig,
}

/// A point-in-time reading of one lane's counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsLaneSnapshot {
    pub lane: &'static str,
    pub threads_configured: usize,
    pub threads_live: usize,
    pub queue_capacity: usize,
    pub deadline_ms: u64,
    pub queued: usize,
    pub in_flight: usize,
    pub total: u64,
    pub resolved: u64,
    pub failed: u64,
    pub timeouts: u64,
    pub saturations: u64,
    pub abandoned: u64,
}

/// OS-backed `reqwest` resolver on a dedicated bounded thread lane.
///
/// Cheap to clone (one `Arc`); clones share the lane.
#[derive(Clone)]
pub struct OsDnsResolver {
    inner: Arc<Inner>,
}

impl fmt::Debug for OsDnsResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OsDnsResolver")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

/// Decrements a counter on drop, so a panicking lookup cannot leak a count.
struct CountGuard<'a>(&'a AtomicUsize);

impl Drop for CountGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl OsDnsResolver {
    /// A lane resolving through the OS resolver.
    pub fn new(config: DnsLaneConfig) -> Self {
        Self::with_lookup(config, Arc::new(os_lookup))
    }

    /// A lane whose threads run `lookup` instead of `getaddrinfo` (tests).
    pub fn with_lookup(config: DnsLaneConfig, lookup: LookupFn) -> Self {
        let threads = config.threads.max(1);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(config.queue_capacity);
        let rx = Arc::new(Mutex::new(rx));
        let counters = Arc::new(Counters::default());
        for i in 0..threads {
            let rx = Arc::clone(&rx);
            let counters_t = Arc::clone(&counters);
            let lookup = Arc::clone(&lookup);
            counters.threads_live.fetch_add(1, Ordering::Relaxed);
            let spawned = std::thread::Builder::new()
                .name(format!("{}-{i}", config.thread_name_prefix))
                .spawn(move || lane_worker(&rx, &counters_t, &lookup));
            if let Err(e) = spawned {
                counters.threads_live.fetch_sub(1, Ordering::Relaxed);
                tracing::warn!(
                    "dns-lane: failed to spawn resolver thread {}-{i}: {e}",
                    config.thread_name_prefix
                );
            }
        }
        Self {
            inner: Arc::new(Inner {
                tx,
                counters,
                config: DnsLaneConfig { threads, ..config },
            }),
        }
    }

    /// Resolve `host` within the lane's configured deadline.
    pub async fn lookup(&self, host: &str) -> Result<Vec<SocketAddr>, DnsLaneError> {
        self.lookup_within(host, self.inner.config.deadline).await
    }

    /// Resolve `host` within `deadline`, never longer than the lane's own.
    pub async fn lookup_within(
        &self,
        host: &str,
        deadline: Duration,
    ) -> Result<Vec<SocketAddr>, DnsLaneError> {
        let deadline = deadline.min(self.inner.config.deadline);
        let c = &self.inner.counters;
        c.total.fetch_add(1, Ordering::Relaxed);
        if c.threads_live.load(Ordering::Relaxed) == 0 {
            return Err(DnsLaneError::Unavailable {
                host: host.to_string(),
            });
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        c.queued.fetch_add(1, Ordering::Relaxed);
        match self.inner.tx.try_send(Job {
            host: host.to_string(),
            reply,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                c.queued.fetch_sub(1, Ordering::Relaxed);
                c.saturations.fetch_add(1, Ordering::Relaxed);
                return Err(DnsLaneError::Saturated {
                    host: host.to_string(),
                    queue_capacity: self.inner.config.queue_capacity,
                });
            }
            Err(TrySendError::Disconnected(_)) => {
                c.queued.fetch_sub(1, Ordering::Relaxed);
                return Err(DnsLaneError::Unavailable {
                    host: host.to_string(),
                });
            }
        }
        match tokio::time::timeout(deadline, rx).await {
            Ok(Ok(Ok(addrs))) => Ok(addrs),
            Ok(Ok(Err(source))) => Err(DnsLaneError::Lookup {
                host: host.to_string(),
                source,
            }),
            // The worker dropped the reply without answering (it died).
            Ok(Err(_)) => Err(DnsLaneError::Unavailable {
                host: host.to_string(),
            }),
            Err(_) => {
                c.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(DnsLaneError::Timeout {
                    host: host.to_string(),
                    deadline,
                })
            }
        }
    }

    /// Time an OS lookup of `host` through this lane, capped at `cap` — the
    /// egress discriminator's DNS-vs-TCP evidence.
    pub async fn probe(&self, host: &str, cap: Duration) -> DnsProbe {
        let started = Instant::now();
        let result = self.lookup_within(host, cap).await;
        let elapsed = started.elapsed();
        let os_resolve_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        let (addrs_n, error_class, error, class) = match &result {
            Ok(addrs) if addrs.is_empty() => (Some(0), None, None, "dns"),
            Ok(addrs) if elapsed >= DNS_SLOW_THRESHOLD => (Some(addrs.len()), None, None, "dns"),
            Ok(addrs) => (Some(addrs.len()), None, None, "tcp"),
            Err(e) => (
                None,
                Some(e.class()),
                Some(render_chain(e)),
                // A saturated or unavailable LANE says nothing about the OS
                // resolver, so it cannot classify the failure either way.
                match e {
                    DnsLaneError::Saturated { .. } | DnsLaneError::Unavailable { .. } => "unknown",
                    _ => "dns",
                },
            ),
        };
        DnsProbe {
            host: host.to_string(),
            os_resolve_ms,
            addrs_n,
            error_class,
            error,
            class,
            slow_threshold_ms: DNS_SLOW_THRESHOLD.as_millis() as u64,
            lane: self.snapshot(),
        }
    }

    /// Current lane counters.
    pub fn snapshot(&self) -> DnsLaneSnapshot {
        let c = &self.inner.counters;
        DnsLaneSnapshot {
            lane: "dns",
            threads_configured: self.inner.config.threads,
            threads_live: c.threads_live.load(Ordering::Relaxed),
            queue_capacity: self.inner.config.queue_capacity,
            deadline_ms: self
                .inner
                .config
                .deadline
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            queued: c.queued.load(Ordering::Relaxed),
            in_flight: c.in_flight.load(Ordering::Relaxed),
            total: c.total.load(Ordering::Relaxed),
            resolved: c.resolved.load(Ordering::Relaxed),
            failed: c.failed.load(Ordering::Relaxed),
            timeouts: c.timeouts.load(Ordering::Relaxed),
            saturations: c.saturations.load(Ordering::Relaxed),
            abandoned: c.abandoned.load(Ordering::Relaxed),
        }
    }
}

fn render_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

fn lane_worker(rx: &Mutex<Receiver<Job>>, counters: &Counters, lookup: &LookupFn) {
    loop {
        // The lock is held only while waiting for the next job; the lookup runs
        // unlocked so the other lane threads keep draining the queue.
        let job = {
            let guard = rx.lock().unwrap_or_else(|p| p.into_inner());
            guard.recv()
        };
        let Ok(job) = job else {
            // Sender gone: the resolver was dropped.
            counters.threads_live.fetch_sub(1, Ordering::Relaxed);
            return;
        };
        counters.queued.fetch_sub(1, Ordering::Relaxed);
        if job.reply.is_closed() {
            // The caller hit its deadline while this job waited; running the
            // lookup would only spend a lane thread on an answer nobody reads.
            counters.abandoned.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        counters.in_flight.fetch_add(1, Ordering::Relaxed);
        let result = {
            let _in_flight = CountGuard(&counters.in_flight);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lookup(&job.host)))
                .unwrap_or_else(|_| Err(io::Error::other("dns-lane lookup panicked")))
        };
        if result.is_ok() {
            counters.resolved.fetch_add(1, Ordering::Relaxed);
        } else {
            counters.failed.fetch_add(1, Ordering::Relaxed);
        }
        let _ = job.reply.send(result);
    }
}

impl reqwest::dns::Resolve for OsDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            match this.lookup(&host).await {
                Ok(addrs) => Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs),
                Err(e) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

/// The result of timing one OS lookup for the egress discriminator.
///
/// `class`: `tcp` — the OS resolved the host quickly, so the failed request's
/// problem was after DNS (connect / TLS / a stale pooled socket); `dns` — the OS
/// resolver failed, returned nothing, or was slower than
/// [`DNS_SLOW_THRESHOLD`]; `unknown` — the lane itself could not run the probe.
#[derive(Debug, Clone, Serialize)]
pub struct DnsProbe {
    pub host: String,
    pub os_resolve_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addrs_n: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub class: &'static str,
    pub slow_threshold_ms: u64,
    pub lane: DnsLaneSnapshot,
}

// ---------------------------------------------------------------------------
// The shared lane
// ---------------------------------------------------------------------------

static SHARED: OnceLock<Arc<OsDnsResolver>> = OnceLock::new();

/// The process-wide resolver every runner HTTP client uses. Built on first use.
pub fn shared() -> Arc<OsDnsResolver> {
    Arc::clone(SHARED.get_or_init(|| Arc::new(OsDnsResolver::new(DnsLaneConfig::default()))))
}

/// The shared lane's counters, or `None` when nothing has used the lane yet —
/// reporting must not spawn four threads just to say they are idle.
pub fn shared_snapshot() -> Option<DnsLaneSnapshot> {
    SHARED.get().map(|r| r.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_config(
        threads: usize,
        queue: usize,
        deadline: Duration,
        prefix: &str,
    ) -> DnsLaneConfig {
        DnsLaneConfig {
            threads,
            queue_capacity: queue,
            deadline,
            thread_name_prefix: prefix.to_string(),
        }
    }

    /// A lookup that blocks until released, recording the thread it ran on.
    fn blocking_lookup() -> (LookupFn, mpsc::Sender<()>, Arc<Mutex<Vec<String>>>) {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let threads_c = Arc::clone(&threads);
        let f: LookupFn = Arc::new(move |_host: &str| {
            threads_c.lock().unwrap().push(
                std::thread::current()
                    .name()
                    .unwrap_or("<unnamed>")
                    .to_string(),
            );
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10));
            Ok(vec![SocketAddr::from(([127, 0, 0, 1], 0))])
        });
        (f, release_tx, threads)
    }

    async fn wait_until(mut cond: impl FnMut() -> bool) {
        let start = Instant::now();
        while !cond() {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "condition never held"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn resolves_localhost_through_the_os_lane() {
        let lane = OsDnsResolver::new(test_config(
            1,
            4,
            Duration::from_secs(4),
            "dns-lane-t-local",
        ));
        let addrs = lane
            .lookup("localhost")
            .await
            .expect("localhost must resolve");
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback()), "{addrs:?}");
        let snap = lane.snapshot();
        assert_eq!(snap.resolved, 1);
        assert_eq!(snap.in_flight, 0);
        assert_eq!(snap.queued, 0);
    }

    #[tokio::test]
    async fn shared_lane_resolves_localhost() {
        let addrs = shared().lookup("localhost").await.expect("localhost");
        assert!(!addrs.is_empty());
        assert!(shared_snapshot().is_some());
    }

    /// Saturation refuses IMMEDIATELY with the typed error, and the lookups that
    /// did run ran on the dedicated lane threads — not tokio's blocking pool.
    #[tokio::test]
    async fn saturated_lane_refuses_without_waiting_for_the_deadline() {
        let (lookup, release, threads) = blocking_lookup();
        let deadline = Duration::from_secs(3);
        let lane =
            OsDnsResolver::with_lookup(test_config(1, 1, deadline, "dns-lane-t-sat"), lookup);

        let l1 = lane.clone();
        let first = tokio::spawn(async move { l1.lookup("a.test").await });
        wait_until(|| lane.snapshot().in_flight == 1).await;
        let l2 = lane.clone();
        let second = tokio::spawn(async move { l2.lookup("b.test").await });
        wait_until(|| lane.snapshot().queued == 1).await;

        let started = Instant::now();
        let third = lane.lookup("c.test").await;
        let waited = started.elapsed();
        let err = third.expect_err("a full lane must refuse");
        assert!(matches!(err, DnsLaneError::Saturated { .. }), "{err:?}");
        assert!(err.to_string().starts_with("dns-lane saturated"), "{err}");
        assert!(
            waited < Duration::from_millis(500),
            "refusal waited {waited:?}"
        );
        assert_eq!(lane.snapshot().saturations, 1);

        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(first.await.unwrap().is_ok());
        assert!(second.await.unwrap().is_ok());
        let names = threads.lock().unwrap().clone();
        assert_eq!(names.len(), 2);
        assert!(
            names.iter().all(|n| n.starts_with("dns-lane-t-sat-")),
            "lookups must run on the dedicated lane, not a blocking-pool thread: {names:?}"
        );
    }

    #[tokio::test]
    async fn stuck_lookup_returns_typed_timeout_at_the_deadline() {
        let (lookup, release, _threads) = blocking_lookup();
        let deadline = Duration::from_millis(150);
        let lane =
            OsDnsResolver::with_lookup(test_config(1, 4, deadline, "dns-lane-t-timeout"), lookup);
        let started = Instant::now();
        let err = lane.lookup("stuck.test").await.expect_err("must time out");
        let waited = started.elapsed();
        assert!(matches!(err, DnsLaneError::Timeout { .. }), "{err:?}");
        assert!(err.to_string().starts_with("dns-lane timeout"), "{err}");
        assert!(
            waited >= deadline && waited < Duration::from_secs(2),
            "{waited:?}"
        );
        assert_eq!(lane.snapshot().timeouts, 1);
        release.send(()).unwrap();
    }

    #[tokio::test]
    async fn lookup_error_keeps_the_os_cause_in_the_chain() {
        let lookup: LookupFn =
            Arc::new(|_h: &str| Err(io::Error::new(io::ErrorKind::NotFound, "host is unknown")));
        let lane = OsDnsResolver::with_lookup(
            test_config(1, 4, Duration::from_secs(1), "dns-lane-t-err"),
            lookup,
        );
        let err = lane.lookup("nope.test").await.expect_err("fails");
        assert_eq!(err.class(), "lookup_failed");
        assert_eq!(
            render_chain(&err),
            "dns-lane lookup failed for nope.test: host is unknown"
        );

        let probe = lane.probe("nope.test", Duration::from_secs(1)).await;
        assert_eq!(probe.class, "dns");
        assert_eq!(probe.error_class, Some("lookup_failed"));
        assert!(probe.addrs_n.is_none());
    }

    #[tokio::test]
    async fn fast_probe_classifies_as_tcp() {
        let lookup: LookupFn = Arc::new(|_h: &str| Ok(vec![SocketAddr::from(([10, 0, 0, 1], 0))]));
        let lane = OsDnsResolver::with_lookup(
            test_config(1, 4, Duration::from_secs(1), "dns-lane-t-probe"),
            lookup,
        );
        let probe = lane.probe("coord.test", Duration::from_secs(2)).await;
        assert_eq!(probe.class, "tcp");
        assert_eq!(probe.addrs_n, Some(1));
        let v = serde_json::to_value(&probe).unwrap();
        assert_eq!(v["lane"]["lane"], "dns");
        assert!(v.get("error").is_none());
    }

    /// The typed error survives reqwest's error wrapping — so the egress WARN
    /// and the 502 envelope name the class instead of "operation timed out".
    #[tokio::test]
    async fn reqwest_error_chain_names_the_lane_class() {
        let lookup: LookupFn =
            Arc::new(|_h: &str| Err(io::Error::new(io::ErrorKind::NotFound, "host is unknown")));
        let lane = Arc::new(OsDnsResolver::with_lookup(
            test_config(1, 4, Duration::from_secs(1), "dns-lane-t-chain"),
            lookup,
        ));
        let client = reqwest::Client::builder() // reqwest-client-ok: test of the resolver itself
            .dns_resolver(lane)
            .connect_timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();
        let err = client
            .get("http://unresolvable.test/")
            .send()
            .await
            .expect_err("must fail to resolve");
        assert!(err.is_connect(), "{err:?}");
        let chain = render_chain(&err);
        assert!(chain.contains("dns-lane lookup failed"), "{chain}");
    }
}
