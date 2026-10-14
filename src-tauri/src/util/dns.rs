//! The process-wide DNS resolver every FIRST-PARTY runner `reqwest` client
//! resolves through: the OS resolver, run on a dedicated, bounded thread lane.
//!
//! "First-party" is the honest scope. Third-party crates that build their own
//! client out of the same `reqwest 0.13` get reqwest's default `GaiResolver`
//! and cannot be routed here unless they expose an injection point — see
//! [`crate::util::http`]'s "Reach" section for which ones do.
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
//! # A lane with no worker must never be permanent
//!
//! Every lookup in the process goes through one shared lane, so a lane that
//! spawned NO worker thread would answer [`DnsLaneError::Unavailable`] for
//! every lookup by every client — coord, the LLM providers, the embedding
//! service, UI Bridge — for the life of the process, and an agent may not
//! restart a runner (served policy `production-and-cost` `runner-lifecycle`).
//! Before this module existed, thread starvation made DNS *slow*
//! (`GaiResolver` on the blocking pool), never fatal. Two guards keep that
//! true:
//!
//! * [`shared`] refuses to CACHE a lane with zero live threads, so the next
//!   client build tries again;
//! * [`OsDnsResolver::lookup_within`] attempts a bounded inline respawn
//!   ([`MAX_RESPAWN_ATTEMPTS`]) before answering `Unavailable`, so a lane
//!   already installed in a live client recovers once spawning is possible.
//!
//! # Dials
//!
//! The lane's shape and the connect-phase ceiling are read from the
//! environment, because on this fleet a running runner cannot be restarted to
//! change a constant: [`ENV_LANE_THREADS`], [`ENV_LANE_QUEUE`],
//! [`ENV_LANE_DEADLINE_MS`] and `util::http`'s
//! [`crate::util::http::ENV_CONNECT_TIMEOUT_MS`]. The resolved values are
//! logged once and reported in every [`DnsLaneSnapshot`], so the envelope says
//! what was actually in force rather than what this file's defaults say.
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

/// Overrides [`DEFAULT_LANE_THREADS`]. `0` and an unparseable value are
/// ignored (the default stands); the lane always has at least one thread.
pub const ENV_LANE_THREADS: &str = "QONTINUI_DNS_LANE_THREADS";

/// Overrides [`DEFAULT_QUEUE_CAPACITY`]. `0` and unparseable are ignored.
pub const ENV_LANE_QUEUE: &str = "QONTINUI_DNS_LANE_QUEUE";

/// Overrides [`DEFAULT_LOOKUP_DEADLINE`], in milliseconds. `0` and unparseable
/// are ignored.
///
/// The lever exists because the effective DNS budget is otherwise hardcoded: a
/// box whose cold `getaddrinfo` takes 5–6s (a VPN/NRPT forwarder
/// fall-through on Windows) would fail every first request with
/// `dns-lane timeout` and have nothing to turn.
pub const ENV_LANE_DEADLINE_MS: &str = "QONTINUI_DNS_LANE_DEADLINE_MS";

/// How many times ONE lane will try to bring a worker back before it gives up
/// and answers [`DnsLaneError::Unavailable`] for good. Bounded so a
/// permanently failing `thread::Builder::spawn` cannot thrash: each attempt
/// costs one failed spawn, and the shared lane is rebuilt from scratch by
/// [`shared`] anyway once it is uncached.
pub const MAX_RESPAWN_ATTEMPTS: u64 = 16;

/// The blocking resolution a lane thread performs. Injectable so tests can
/// substitute a lookup that blocks, fails, or changes its answer.
pub type LookupFn = Arc<dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync>;

/// How a lane starts a worker thread. Injectable so a test can simulate thread
/// exhaustion — the [`DnsLaneError::Unavailable`] path above — without
/// exhausting the machine's threads.
pub type SpawnFn =
    Arc<dyn Fn(String, Box<dyn FnOnce() + Send + 'static>) -> io::Result<()> + Send + Sync>;

/// The production lookup: `getaddrinfo` via `ToSocketAddrs`.
///
/// Port `0` on purpose: reqwest substitutes the URL's explicit port, or the
/// scheme default, for a resolved port of `0`.
pub fn os_lookup(host: &str) -> io::Result<Vec<SocketAddr>> {
    (host, 0u16).to_socket_addrs().map(|it| it.collect())
}

/// The production spawner: one named OS thread per worker.
pub fn os_spawn(name: String, body: Box<dyn FnOnce() + Send + 'static>) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name)
        .spawn(body)
        .map(|_| ())
}

fn env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(v) if v > 0 => v,
            _ => default,
        },
        Err(_) => default,
    }
}

/// The shared lane's shape, from the environment with this file's constants as
/// defaults. Resolved fresh on each call (cheap) but only ever used once, when
/// [`shared`] first builds the lane.
pub fn shared_config() -> DnsLaneConfig {
    DnsLaneConfig {
        threads: env_usize(ENV_LANE_THREADS, DEFAULT_LANE_THREADS),
        queue_capacity: env_usize(ENV_LANE_QUEUE, DEFAULT_QUEUE_CAPACITY),
        deadline: Duration::from_millis(env_usize(
            ENV_LANE_DEADLINE_MS,
            DEFAULT_LOOKUP_DEADLINE.as_millis() as usize,
        ) as u64),
        thread_name_prefix: SHARED_LANE_THREAD_PREFIX.to_string(),
    }
}

/// RFC 6761 §6.3: `localhost` and any name under `.localhost` are loopback and
/// need no network DNS at all. A trailing root dot is accepted.
pub fn is_localhost_name(host: &str) -> bool {
    let h = host
        .trim_end_matches('.')
        .trim_matches(|c| c == '[' || c == ']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // Byte-safe rather than sliced by a computed index: this runs inside the
    // resolver, and a non-char-boundary slice of an IDN host would panic there.
    h.to_ascii_lowercase().ends_with(".localhost")
}

/// The answer [`is_localhost_name`] short-circuits to. IPv4 FIRST on purpose:
/// on Windows `localhost` resolves to `::1` ahead of `127.0.0.1`, and the
/// runner's own services bind the IPv4 loopback only, so an IPv6-first answer
/// pays a doomed connect before the socket that answers.
fn loopback_addrs() -> Vec<SocketAddr> {
    vec![
        SocketAddr::from(([127, 0, 0, 1], 0)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
    ]
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

    /// The egress discriminator's class for this error — see
    /// [`probe_class_for`].
    pub fn probe_class(&self) -> &'static str {
        probe_class_for(self.class())
    }
}

/// The egress discriminator class (`dns` / `unknown`) implied by a lane error
/// [`class`](DnsLaneError::class). A saturated or unavailable LANE says nothing
/// about the OS resolver, so it cannot classify the failure either way; a
/// timeout or a failed lookup does point at DNS.
///
/// Shared so [`OsDnsResolver::probe`] and the error-chain short-circuit in
/// `util::egress_context` cannot drift apart.
pub fn probe_class_for(class: &str) -> &'static str {
    match class {
        "saturated" | "unavailable" => "unknown",
        _ => "dns",
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

/// Decrements `queued` exactly once, whichever way the job leaves the queue:
/// a worker dequeuing it, or the channel being dropped with the job still
/// buffered (which is what happens when the resolver is dropped — the
/// `Receiver` lives in [`Inner`] for the respawn path, so buffered jobs are
/// destroyed rather than delivered). Making the decrement structural is the
/// only way the counter cannot leak on a drop order nobody remembers.
struct QueuedGuard(Arc<Counters>);

impl Drop for QueuedGuard {
    fn drop(&mut self) {
        self.0.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Job {
    host: String,
    reply: tokio::sync::oneshot::Sender<io::Result<Vec<SocketAddr>>>,
    queued: QueuedGuard,
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
    /// Lookups refused because the lane had no live worker to take them — the
    /// signal that the lane itself, not the network, is the fault.
    unavailable: AtomicU64,
    /// Jobs whose caller had already given up (deadline) before a lane thread
    /// picked them up — skipped without running the lookup.
    abandoned: AtomicU64,
    /// Worker respawns ATTEMPTED (bounded by [`MAX_RESPAWN_ATTEMPTS`]) and the
    /// subset that produced a live thread.
    respawn_attempts: AtomicU64,
    respawns: AtomicU64,
}

struct Inner {
    tx: SyncSender<Job>,
    /// Shared with every worker. Held here too so a worker can be respawned
    /// onto the existing queue after a spawn failure (see
    /// [`OsDnsResolver::try_respawn_worker`]).
    rx: Arc<Mutex<Receiver<Job>>>,
    lookup: LookupFn,
    spawn: SpawnFn,
    counters: Arc<Counters>,
    config: DnsLaneConfig,
}

/// A point-in-time reading of one lane's counters.
///
/// ⚠️ **The outcome buckets are NOT a partition of `total`.** One lookup can
/// increment several: a timeout bumps `timeouts` and then, when the worker
/// eventually reaches the job, `abandoned` — or `resolved`/`failed` if it had
/// already started. So `resolved + failed + timeouts + saturations +
/// unavailable + abandoned` may exceed `total`, and a difference is not a lost
/// lookup. `total` is the only count of CALLS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsLaneSnapshot {
    pub lane: &'static str,
    pub threads_configured: usize,
    pub threads_live: usize,
    pub queue_capacity: usize,
    pub deadline_ms: u64,
    /// The connect-phase ceiling in force for clients built through
    /// `util::http` — not a property of the lane, reported beside it because
    /// the two dials are only meaningful together: a lane deadline above the
    /// connect timeout can never be observed, and both are env-overridable, so
    /// an envelope that named only one could not say what was in force.
    pub http_connect_timeout_ms: u64,
    pub queued: usize,
    pub in_flight: usize,
    pub total: u64,
    pub resolved: u64,
    pub failed: u64,
    pub timeouts: u64,
    pub saturations: u64,
    pub unavailable: u64,
    pub abandoned: u64,
    pub respawn_attempts: u64,
    pub respawns: u64,
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
        Self::with_lookup_and_spawn(config, lookup, Arc::new(os_spawn))
    }

    /// As [`with_lookup`](Self::with_lookup) with an injected thread spawner —
    /// the seam that lets a test reproduce "no worker could be started" without
    /// exhausting the machine's threads.
    pub fn with_lookup_and_spawn(config: DnsLaneConfig, lookup: LookupFn, spawn: SpawnFn) -> Self {
        let threads = config.threads.max(1);
        let queue_capacity = config.queue_capacity.max(1);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(queue_capacity);
        let rx = Arc::new(Mutex::new(rx));
        let counters = Arc::new(Counters::default());
        let me = Self {
            inner: Arc::new(Inner {
                tx,
                rx,
                lookup,
                spawn,
                counters,
                config: DnsLaneConfig {
                    threads,
                    queue_capacity,
                    ..config
                },
            }),
        };
        for i in 0..threads {
            me.spawn_worker(&format!("{}-{i}", me.inner.config.thread_name_prefix));
        }
        me
    }

    /// Start one worker on this lane's existing queue. Returns whether a thread
    /// is now live.
    fn spawn_worker(&self, name: &str) -> bool {
        let inner = &self.inner;
        let counters_t = Arc::clone(&inner.counters);
        let rx = Arc::clone(&inner.rx);
        let lookup = Arc::clone(&inner.lookup);
        // Counted BEFORE the spawn so a lookup racing the spawn never sees a
        // lane that looks dead while a worker is starting; undone on failure.
        inner.counters.threads_live.fetch_add(1, Ordering::Relaxed);
        let body: Box<dyn FnOnce() + Send + 'static> =
            Box::new(move || lane_worker(&rx, &counters_t, &lookup));
        match (inner.spawn)(name.to_string(), body) {
            Ok(()) => true,
            Err(e) => {
                inner.counters.threads_live.fetch_sub(1, Ordering::Relaxed);
                tracing::warn!("dns-lane: failed to spawn resolver thread {name}: {e}");
                false
            }
        }
    }

    /// Live worker threads on this lane. Zero means every lookup would be
    /// refused with [`DnsLaneError::Unavailable`], which is why neither
    /// [`shared`] nor [`lookup_within`](Self::lookup_within) treats that state
    /// as final.
    pub fn threads_live(&self) -> usize {
        self.inner.counters.threads_live.load(Ordering::Relaxed)
    }

    /// The counters, detached from the resolver, so a test can read them AFTER
    /// the resolver itself has been dropped (the drop-order invariant
    /// `QueuedGuard` exists for).
    #[cfg(test)]
    fn counters_handle(&self) -> Arc<Counters> {
        Arc::clone(&self.inner.counters)
    }

    /// Bring a worker back on a lane that has none, at most
    /// [`MAX_RESPAWN_ATTEMPTS`] times for the lane's whole life. Returns
    /// whether a worker is live afterwards.
    ///
    /// The bound matters both ways: without a respawn the lane is dead for the
    /// process's life, and without a cap a spawn that always fails would be
    /// retried on every single lookup.
    fn try_respawn_worker(&self) -> bool {
        let c = &self.inner.counters;
        let attempt = loop {
            let n = c.respawn_attempts.load(Ordering::Relaxed);
            if n >= MAX_RESPAWN_ATTEMPTS {
                return false;
            }
            if c.respawn_attempts
                .compare_exchange(n, n + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break n + 1;
            }
        };
        // Another caller may have won the race and already revived the lane.
        if c.threads_live.load(Ordering::Relaxed) > 0 {
            return true;
        }
        let name = format!("{}-r{attempt}", self.inner.config.thread_name_prefix);
        if self.spawn_worker(&name) {
            c.respawns.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                "dns-lane: revived the lane with worker {name} after it had no live thread \
                 (attempt {attempt}/{MAX_RESPAWN_ATTEMPTS})"
            );
            true
        } else {
            false
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
        if c.threads_live.load(Ordering::Relaxed) == 0 && !self.try_respawn_worker() {
            // The lane has no worker and could not get one. Refusing here is
            // the LAST resort, not the first: a cached lane in this state kills
            // every HTTP client in the process (see the module docs).
            c.unavailable.fetch_add(1, Ordering::Relaxed);
            return Err(DnsLaneError::Unavailable {
                host: host.to_string(),
            });
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        // The guard rides WITH the job, so the count is released by whoever
        // consumes or destroys it — never left behind (see `QueuedGuard`).
        c.queued.fetch_add(1, Ordering::Relaxed);
        let queued = QueuedGuard(Arc::clone(&self.inner.counters));
        match self.inner.tx.try_send(Job {
            host: host.to_string(),
            reply,
            queued,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                c.saturations.fetch_add(1, Ordering::Relaxed);
                return Err(DnsLaneError::Saturated {
                    host: host.to_string(),
                    queue_capacity: self.inner.config.queue_capacity,
                });
            }
            Err(TrySendError::Disconnected(_)) => {
                c.unavailable.fetch_add(1, Ordering::Relaxed);
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
            Ok(Err(_)) => {
                c.unavailable.fetch_add(1, Ordering::Relaxed);
                Err(DnsLaneError::Unavailable {
                    host: host.to_string(),
                })
            }
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
                e.probe_class(),
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
            http_connect_timeout_ms: crate::util::http::connect_timeout().as_millis() as u64,
            queued: c.queued.load(Ordering::Relaxed),
            in_flight: c.in_flight.load(Ordering::Relaxed),
            total: c.total.load(Ordering::Relaxed),
            resolved: c.resolved.load(Ordering::Relaxed),
            failed: c.failed.load(Ordering::Relaxed),
            timeouts: c.timeouts.load(Ordering::Relaxed),
            saturations: c.saturations.load(Ordering::Relaxed),
            unavailable: c.unavailable.load(Ordering::Relaxed),
            abandoned: c.abandoned.load(Ordering::Relaxed),
            respawn_attempts: c.respawn_attempts.load(Ordering::Relaxed),
            respawns: c.respawns.load(Ordering::Relaxed),
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
        let Job {
            host,
            reply,
            queued,
        } = job;
        // Dequeued: it is no longer waiting, whatever happens to it next.
        drop(queued);
        if reply.is_closed() {
            // The caller hit its deadline while this job waited; running the
            // lookup would only spend a lane thread on an answer nobody reads.
            counters.abandoned.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        counters.in_flight.fetch_add(1, Ordering::Relaxed);
        let result = {
            let _in_flight = CountGuard(&counters.in_flight);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lookup(&host)))
                .unwrap_or_else(|_| Err(io::Error::other("dns-lane lookup panicked")))
        };
        if result.is_ok() {
            counters.resolved.fetch_add(1, Ordering::Relaxed);
        } else {
            counters.failed.fetch_add(1, Ordering::Relaxed);
        }
        let _ = reply.send(result);
    }
}

impl reqwest::dns::Resolve for OsDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        // Head-of-line blocking: the lane is a FIXED number of threads, so a
        // `localhost` lookup — the runner's own loopback services, the UI
        // Bridge, the embedding service — could otherwise sit behind four
        // wedged external lookups during exactly the DNS outage this module
        // exists to survive. It never needs network DNS, so it never enters
        // the queue. (An IP literal already bypasses the resolver entirely in
        // hyper-util, so `127.0.0.1` sites cost nothing here either.)
        if is_localhost_name(&host) {
            return Box::pin(async move {
                Ok(Box::new(loopback_addrs().into_iter()) as reqwest::dns::Addrs)
            });
        }
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

/// `Mutex<Option<_>>` rather than a `OnceLock`: a `OnceLock` would cache the
/// FIRST lane built, including one that spawned no worker — permanently
/// refusing every lookup in the process (see the module docs).
static SHARED: Mutex<Option<Arc<OsDnsResolver>>> = Mutex::new(None);

/// The process-wide resolver every first-party runner HTTP client uses. Built
/// on first use, and — the load-bearing part — **not cached unless it has a
/// live worker**, so a build that happened during thread exhaustion is retried
/// by the next client build instead of poisoning the process.
pub fn shared() -> Arc<OsDnsResolver> {
    resolve_shared(&SHARED, &|| {
        let config = shared_config();
        log_resolved_dials(&config);
        Arc::new(OsDnsResolver::new(config))
    })
}

/// The caching policy [`shared`] applies, over an injectable slot and builder
/// so it can be tested without touching the process-wide lane.
fn resolve_shared(
    slot: &Mutex<Option<Arc<OsDnsResolver>>>,
    build: &dyn Fn() -> Arc<OsDnsResolver>,
) -> Arc<OsDnsResolver> {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(live) = guard.as_ref() {
        if live.threads_live() > 0 {
            return Arc::clone(live);
        }
        // Cached but dead: drop it so nothing else picks it up.
        tracing::warn!(
            "dns-lane: the shared lane has no live worker thread; rebuilding it rather than \
             serving a resolver that refuses every lookup"
        );
        *guard = None;
    }
    let fresh = build();
    if fresh.threads_live() > 0 {
        *guard = Some(Arc::clone(&fresh));
    } else {
        tracing::error!(
            "dns-lane: could not start ANY resolver thread; this lane is NOT cached so the next \
             client build retries, and each lookup on it attempts a bounded respawn \
             (set {ENV_LANE_THREADS}/{ENV_LANE_QUEUE} if the lane's shape is the problem)"
        );
    }
    fresh
}

/// Log the dials actually in force, once per process, so a box running with an
/// override does not have to be inferred from behaviour.
fn log_resolved_dials(config: &DnsLaneConfig) {
    static LOGGED: OnceLock<()> = OnceLock::new();
    if LOGGED.set(()).is_err() {
        return;
    }
    let src = |key: &str| {
        if std::env::var(key).is_ok() {
            "env"
        } else {
            "default"
        }
    };
    tracing::info!(
        "dns-lane: threads={} ({}) queue={} ({}) deadline_ms={} ({}) http_connect_timeout_ms={} ({})",
        config.threads,
        src(ENV_LANE_THREADS),
        config.queue_capacity,
        src(ENV_LANE_QUEUE),
        config.deadline.as_millis(),
        src(ENV_LANE_DEADLINE_MS),
        crate::util::http::connect_timeout().as_millis(),
        src(crate::util::http::ENV_CONNECT_TIMEOUT_MS),
    );
}

/// The shared lane's counters, or `None` when nothing has used the lane yet —
/// reporting must not spawn four threads just to say they are idle.
pub fn shared_snapshot() -> Option<DnsLaneSnapshot> {
    SHARED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|r| r.snapshot())
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

    // -----------------------------------------------------------------
    // F1: a lane that spawns no worker must never be permanent
    // -----------------------------------------------------------------

    /// A spawner that refuses until `allow` is set, so "no thread could be
    /// started" is reproducible without exhausting the machine's threads.
    fn gated_spawn(allow: Arc<std::sync::atomic::AtomicBool>) -> SpawnFn {
        Arc::new(move |name, body| {
            if allow.load(Ordering::Relaxed) {
                os_spawn(name, body)
            } else {
                Err(io::Error::other("spawn refused (test)"))
            }
        })
    }

    fn ok_lookup() -> LookupFn {
        Arc::new(|_h: &str| Ok(vec![SocketAddr::from(([10, 0, 0, 7], 0))]))
    }

    /// The `OnceLock` this replaced would have cached the dead lane FOREVER,
    /// leaving every HTTP client in the process unable to resolve anything
    /// until a runner restart — which agents may not perform.
    #[test]
    fn a_lane_with_no_worker_is_never_cached_and_is_retried() {
        let allow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let builds = Arc::new(AtomicUsize::new(0));
        let slot: Mutex<Option<Arc<OsDnsResolver>>> = Mutex::new(None);
        let builds_b = Arc::clone(&builds);
        let allow_b = Arc::clone(&allow);
        let build = move || {
            builds_b.fetch_add(1, Ordering::Relaxed);
            Arc::new(OsDnsResolver::with_lookup_and_spawn(
                test_config(2, 4, Duration::from_secs(1), "dns-lane-t-nocache"),
                ok_lookup(),
                gated_spawn(Arc::clone(&allow_b)),
            ))
        };

        let first = resolve_shared(&slot, &build);
        assert_eq!(first.threads_live(), 0, "no thread could be spawned");
        assert!(
            slot.lock().unwrap().is_none(),
            "a lane with zero live threads must NOT be cached"
        );

        let second = resolve_shared(&slot, &build);
        assert_eq!(second.threads_live(), 0);
        assert_eq!(
            builds.load(Ordering::Relaxed),
            2,
            "a later client build must get a fresh attempt, not the dead lane"
        );

        // Spawning becomes possible: the next attempt is cached and reused.
        allow.store(true, Ordering::Relaxed);
        let third = resolve_shared(&slot, &build);
        assert!(third.threads_live() > 0, "the lane recovered");
        assert_eq!(builds.load(Ordering::Relaxed), 3);
        let fourth = resolve_shared(&slot, &build);
        assert_eq!(
            builds.load(Ordering::Relaxed),
            3,
            "a healthy lane is cached, not rebuilt per call"
        );
        assert!(Arc::ptr_eq(&third, &fourth));
    }

    /// A lane already installed in a live client cannot be replaced, so it must
    /// heal itself: `lookup_within` respawns a worker instead of answering
    /// `Unavailable` forever.
    #[tokio::test]
    async fn a_lookup_revives_a_lane_that_lost_every_worker() {
        let allow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lane = OsDnsResolver::with_lookup_and_spawn(
            test_config(1, 4, Duration::from_secs(2), "dns-lane-t-revive"),
            ok_lookup(),
            gated_spawn(Arc::clone(&allow)),
        );
        assert_eq!(lane.threads_live(), 0);

        let err = lane.lookup("a.test").await.expect_err("no worker");
        assert!(matches!(err, DnsLaneError::Unavailable { .. }), "{err:?}");
        let snap = lane.snapshot();
        assert_eq!(snap.unavailable, 1, "the Unavailable class has a counter");
        assert_eq!(snap.respawn_attempts, 1, "it TRIED before refusing");
        assert_eq!(snap.respawns, 0);

        allow.store(true, Ordering::Relaxed);
        let addrs = lane
            .lookup("a.test")
            .await
            .expect("the lane must recover once a thread can be spawned");
        assert_eq!(addrs.len(), 1);
        assert!(lane.threads_live() > 0);
        assert_eq!(lane.snapshot().respawns, 1);
    }

    /// A spawn that can never succeed must not be retried on every lookup.
    #[tokio::test]
    async fn respawn_is_bounded() {
        let allow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lane = OsDnsResolver::with_lookup_and_spawn(
            test_config(1, 4, Duration::from_millis(200), "dns-lane-t-bound"),
            ok_lookup(),
            gated_spawn(allow),
        );
        for _ in 0..(MAX_RESPAWN_ATTEMPTS + 5) {
            let err = lane.lookup("a.test").await.expect_err("no worker ever");
            assert!(matches!(err, DnsLaneError::Unavailable { .. }), "{err:?}");
        }
        let snap = lane.snapshot();
        assert_eq!(snap.respawn_attempts, MAX_RESPAWN_ATTEMPTS);
        assert_eq!(snap.respawns, 0);
        assert_eq!(snap.unavailable, MAX_RESPAWN_ATTEMPTS + 5);
    }

    // -----------------------------------------------------------------
    // N10: `queued` must not leak on any drop order
    // -----------------------------------------------------------------

    /// A spawner that reports success without running the body: the lane
    /// believes it has a worker, nothing drains the queue, and dropping the
    /// resolver destroys the buffered jobs instead of delivering them — the
    /// drop order in which a manual decrement in `lane_worker` leaks.
    #[tokio::test]
    async fn queued_does_not_leak_when_buffered_jobs_are_destroyed() {
        let never_runs: SpawnFn = Arc::new(|_n, _b| Ok(()));
        let lane = OsDnsResolver::with_lookup_and_spawn(
            test_config(1, 4, Duration::from_millis(120), "dns-lane-t-leak"),
            ok_lookup(),
            never_runs,
        );
        let counters = lane.counters_handle();
        let err = lane
            .lookup("buffered.test")
            .await
            .expect_err("nothing runs");
        assert!(matches!(err, DnsLaneError::Timeout { .. }), "{err:?}");
        assert_eq!(
            counters.queued.load(Ordering::Relaxed),
            1,
            "the job is still buffered: nothing dequeued it"
        );
        drop(lane);
        assert_eq!(
            counters.queued.load(Ordering::Relaxed),
            0,
            "dropping the lane must release the queued count, not leak it"
        );
    }

    // -----------------------------------------------------------------
    // F7: the dials are readable from the environment
    // -----------------------------------------------------------------

    #[test]
    fn env_usize_takes_an_override_and_ignores_junk() {
        let key = "__QONTINUI_DNS_TEST_USIZE";
        std::env::remove_var(key);
        assert_eq!(env_usize(key, 4), 4);
        std::env::set_var(key, "9");
        assert_eq!(env_usize(key, 4), 9);
        std::env::set_var(key, " 9 ");
        assert_eq!(env_usize(key, 4), 9, "whitespace is trimmed");
        std::env::set_var(key, "0");
        assert_eq!(env_usize(key, 4), 4, "0 is not a usable lane dial");
        std::env::set_var(key, "not-a-number");
        assert_eq!(env_usize(key, 4), 4);
        std::env::remove_var(key);
    }

    /// The snapshot is what the 502 envelope carries, so it must name the
    /// values ACTUALLY in force — including the connect ceiling, which is
    /// meaningless to compare a DNS deadline against if it is not there.
    #[test]
    fn the_snapshot_reports_the_dials_in_force() {
        let lane = OsDnsResolver::with_lookup(
            test_config(3, 7, Duration::from_millis(1234), "dns-lane-t-dials"),
            ok_lookup(),
        );
        let snap = lane.snapshot();
        assert_eq!(snap.threads_configured, 3);
        assert_eq!(snap.queue_capacity, 7);
        assert_eq!(snap.deadline_ms, 1234);
        assert_eq!(
            snap.http_connect_timeout_ms,
            crate::util::http::connect_timeout().as_millis() as u64
        );
    }

    // -----------------------------------------------------------------
    // F8: `localhost` never queues behind an external lookup
    // -----------------------------------------------------------------

    #[test]
    fn localhost_names_are_recognised() {
        for h in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "db.localhost",
            "a.b.LocalHost",
        ] {
            assert!(is_localhost_name(h), "{h}");
        }
        for h in [
            "notlocalhost",
            "localhost.example.com",
            "coord.qontinui.io",
            "",
            "localhostx",
        ] {
            assert!(!is_localhost_name(h), "{h}");
        }
    }

    /// Head-of-line blocking: with every lane thread wedged on an external
    /// lookup, a `localhost` resolve must still answer — it never enters the
    /// queue at all.
    #[tokio::test]
    async fn localhost_resolves_without_touching_the_lane() {
        use reqwest::dns::Resolve;
        let (lookup, release, _threads) = blocking_lookup();
        let lane = OsDnsResolver::with_lookup(
            test_config(1, 1, Duration::from_secs(3), "dns-lane-t-hol"),
            lookup,
        );
        // Wedge the single thread and fill the queue.
        let l1 = lane.clone();
        let wedged = tokio::spawn(async move { l1.lookup("slow.test").await });
        wait_until(|| lane.snapshot().in_flight == 1).await;
        let l2 = lane.clone();
        let queued = tokio::spawn(async move { l2.lookup("slower.test").await });
        wait_until(|| lane.snapshot().queued == 1).await;
        let before = lane.snapshot().total;

        let started = Instant::now();
        let addrs: Vec<SocketAddr> = lane
            .resolve("localhost".parse::<reqwest::dns::Name>().unwrap())
            .await
            .expect("localhost must resolve with the lane fully wedged")
            .collect();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "it waited {:?} — it went through the queue",
            started.elapsed()
        );
        assert!(addrs.iter().all(|a| a.ip().is_loopback()), "{addrs:?}");
        assert!(
            addrs.first().map(|a| a.is_ipv4()).unwrap_or(false),
            "IPv4 first: the runner's own services bind the v4 loopback only: {addrs:?}"
        );
        assert_eq!(
            lane.snapshot().total,
            before,
            "the lane must not have been asked at all"
        );

        release.send(()).unwrap();
        release.send(()).unwrap();
        let _ = wedged.await;
        let _ = queued.await;
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
