//! The egress discriminator: the small, cheap process-level context every
//! coord-egress failure carries, and the periodic INFO baseline that gives it
//! something to be compared against.
//!
//! # What question this exists to answer
//!
//! On 2026-08-02 this runner emitted 5,967 coord-egress failures in a day. The
//! WARNs said `error sending request for url (…)` and nothing else, so the two
//! candidate explanations — *coord was unreachable* versus *this process had
//! run out of sockets / handles and could not open a connection* — were
//! indistinguishable from the logs. Nothing in the runner measured the second
//! one at all: a repo-wide search for `open_sockets`, `fd_count`,
//! `/proc/self/fd` or an egress baseline returned zero hits, and the only
//! `pool_idle` matches were `reqwest::ClientBuilder::pool_idle_timeout`
//! **configuration**, not instrumentation.
//!
//! # Honest framing
//!
//! **This is instrumentation for the NEXT episode, not a fix for an active
//! bleed.** The class has largely abated on its own (08-07 and 08-08 ran at
//! ~1 event/day against 5,967 on 08-02). What makes the instrumentation worth
//! landing anyway is that the standing hypothesis — that the failure rate is
//! governed by runner **process age**, i.e. a slow resource leak rather than a
//! coord-side fault — is falsifiable and currently untested: it predicts
//! recurrence as the live process ages, and [`process_uptime_ms`] plus the
//! handle counts below are exactly the two series needed to confirm or refute
//! it. Without a BASELINE emitted while things are healthy, a count captured
//! during an episode is a number with nothing to compare it to, which is why
//! [`spawn_baseline_logger`] exists alongside the failure-path snapshot.
//!
//! # Measurement honesty
//!
//! The runner ships on Windows and Linux and `/proc/self/fd` is Linux-only.
//! Every counter here is therefore an [`Measured`], never a bare integer: a
//! platform that cannot answer emits a typed `unavailable` string with the
//! reason, never `0`. A silent zero is a confidently-wrong measurement — the
//! exact class this whole plan is about — and would read as "this process holds
//! no sockets" at the moment the truth is "nobody looked".
//!
//! # Cost
//!
//! Called on failure paths, so: no locks, no formatting unless something
//! actually failed, and the fd walk is `read_dir` + one `read_link` per entry
//! bounded by [`FD_CLASSIFY_CEILING`]. Above that ceiling the total is still
//! reported and the socket split degrades to a typed `unavailable` rather than
//! spending thousands of syscalls inside an error handler.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{json, Value};
use tracing::info;

/// Above this many open descriptors the per-fd `read_link` classification is
/// skipped: the total is the load-bearing number, and a failure handler has no
/// business issuing five thousand syscalls to refine it.
const FD_CLASSIFY_CEILING: usize = 4096;

/// How often the healthy-state baseline is emitted at INFO.
///
/// Sized so a multi-day process leaves a readable series without adding a
/// meaningful line count: 96 lines a day.
const BASELINE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

// ---------------------------------------------------------------------------
// Named egress clients
// ---------------------------------------------------------------------------

/// The coord-egress clients this runner owns, each with its own `reqwest`
/// connection pool.
///
/// Named rather than free-form so the in-flight counters below are a fixed
/// array with no allocation and no map lookup on the failure path — and so a
/// reader can tell WHICH pool was saturated, which is the whole point of
/// attributing the count to a client at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EgressClient {
    /// `/coord-mcp` — the MCP JSON-RPC proxy.
    CoordMcpProxy,
    /// The nonce-gated REST read passthroughs (`claims/*`, `agent-*`,
    /// `pr-merge/*`). One pool serves both `ReadProxyCodes` families.
    CoordRead,
    /// The nonce-gated coord WRITE forwarder.
    CoordWrite,
    /// The VCS PR-creation forwarder.
    VcsPr,
    /// `session_message_poller`.
    SessionMessagePoller,
    /// `fleet_policy_poller`.
    FleetPolicyPoller,
    /// `device_jwt_refresher`.
    DeviceJwtRefresher,
}

impl EgressClient {
    /// Every arm, in index order. The array statics below are indexed by
    /// [`EgressClient::index`], so this list and they must stay the same
    /// length — pinned by `client_indices_are_dense_and_unique`.
    pub(crate) const ALL: [EgressClient; 7] = [
        EgressClient::CoordMcpProxy,
        EgressClient::CoordRead,
        EgressClient::CoordWrite,
        EgressClient::VcsPr,
        EgressClient::SessionMessagePoller,
        EgressClient::FleetPolicyPoller,
        EgressClient::DeviceJwtRefresher,
    ];

    /// The stable machine token that appears in the envelope and the log.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EgressClient::CoordMcpProxy => "coord-mcp-proxy",
            EgressClient::CoordRead => "coord-read-proxy",
            EgressClient::CoordWrite => "coord-write-proxy",
            EgressClient::VcsPr => "vcs-pr-proxy",
            EgressClient::SessionMessagePoller => "session-message-poller",
            EgressClient::FleetPolicyPoller => "fleet-policy-poller",
            EgressClient::DeviceJwtRefresher => "device-jwt-refresher",
        }
    }

    fn index(self) -> usize {
        match self {
            EgressClient::CoordMcpProxy => 0,
            EgressClient::CoordRead => 1,
            EgressClient::CoordWrite => 2,
            EgressClient::VcsPr => 3,
            EgressClient::SessionMessagePoller => 4,
            EgressClient::FleetPolicyPoller => 5,
            EgressClient::DeviceJwtRefresher => 6,
        }
    }
}

/// Requests currently between `send()` and a response, per client. Spelled as
/// an explicit array (not `[AtomicUsize::new(0); N]`, which does not exist for
/// a non-`Copy` type) so it is a `static` with no lazy init on the hot path.
static IN_FLIGHT: [AtomicUsize; 7] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// Cumulative transport-layer failures per client since process start.
static FAILURES: [AtomicU64; 7] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Held across one outbound request's `send()` — the connect + write phase, and
/// the one that actually contends for sockets. It is deliberately dropped
/// before the response BODY is streamed: a slow body read holds a connection
/// but is not the saturation this counter exists to see, and folding the two
/// together would make a single large download read as "the pool is full".
/// Decrements on drop, including on the error path and on a cancelled future.
///
/// This is the ONLY honest in-flight number available: `reqwest` exposes no
/// connection-pool introspection whatsoever (no idle count, no checked-out
/// count, no waiter count), so a runner that wants to know whether its own
/// concurrency exploded has to count for itself. See [`POOL_INTROSPECTION`].
pub(crate) struct InFlightGuard(usize);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        IN_FLIGHT[self.0].fetch_sub(1, Ordering::Relaxed);
    }
}

/// Mark one request in flight for `client`. Hold the guard across the `await`.
pub(crate) fn in_flight(client: EgressClient) -> InFlightGuard {
    let i = client.index();
    IN_FLIGHT[i].fetch_add(1, Ordering::Relaxed);
    InFlightGuard(i)
}

/// Record one transport-layer failure for `client`. Returns the new total, so a
/// caller can put "this is the Nth" straight into its WARN.
pub(crate) fn record_failure(client: EgressClient) -> u64 {
    FAILURES[client.index()].fetch_add(1, Ordering::Relaxed) + 1
}

/// Why the envelope reports no pool numbers, spelled out rather than omitted.
///
/// An absent field reads as "nobody instrumented this"; this string says the
/// stronger and true thing — the upstream library has no such API to read, so
/// the in-flight counter above is the substitute, not a lazy approximation of
/// something we could have measured.
const POOL_INTROSPECTION: &str =
    "unavailable: reqwest exposes no connection-pool counters (idle/checked-out/waiters); \
     `in_flight` below is this runner's own count";

// ---------------------------------------------------------------------------
// Process uptime — the same mechanism `/health` already uses
// ---------------------------------------------------------------------------

/// Set once, from the same place `/health`'s `uptimeSeconds` is anchored
/// (`MCPState::started_at`), so the two surfaces cannot disagree about how old
/// this process is.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

/// Anchor the uptime clock. Idempotent — a second call is a no-op, so a test
/// harness or a second router build cannot silently reset the series.
pub(crate) fn init_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

/// Milliseconds since [`init_process_start`], or `None` when it was never
/// called (a unit test, a CLI bin). `None` renders as JSON `null`: an
/// un-anchored clock is UNKNOWN, and reporting `0` would assert a
/// freshly-started process, which is the single most misleading answer
/// available for a hypothesis about process AGE.
pub(crate) fn process_uptime_ms() -> Option<u64> {
    PROCESS_START
        .get()
        .map(|t| t.elapsed().as_millis().min(u64::MAX as u128) as u64)
}

// ---------------------------------------------------------------------------
// Descriptor / handle counts
// ---------------------------------------------------------------------------

/// A count that was either taken or explicitly not taken. There is no third
/// spelling, and in particular there is no `0` meaning "we did not look".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Measured {
    Counted(u64),
    /// Why this platform / this call could not answer.
    Unavailable(&'static str),
}

impl Measured {
    fn to_json(&self) -> Value {
        match self {
            Measured::Counted(n) => json!(n),
            Measured::Unavailable(why) => json!({ "unavailable": why }),
        }
    }

    /// The count, or `None` when it was not taken — the only lossless way to
    /// hand a [`Measured`] to code that models UNKNOWN as `Option`.
    pub(crate) fn counted(&self) -> Option<u64> {
        match self {
            Measured::Counted(n) => Some(*n),
            Measured::Unavailable(_) => None,
        }
    }
}

/// Every open descriptor / handle this process holds, and how many of them are
/// sockets.
#[derive(Debug, Clone)]
pub(crate) struct HandleCensus {
    /// Linux: entries in `/proc/self/fd`. Windows: `GetProcessHandleCount`,
    /// which counts ALL kernel handles (files, events, threads, sockets), not
    /// only sockets — the field name says `handles` there for that reason.
    pub(crate) open: Measured,
    /// Linux only: descriptors whose `read_link` target starts with `socket:`.
    /// Windows has no cheap equivalent (it needs an `NtQuerySystemInformation`
    /// handle-table walk), so it reports `unavailable` rather than a guess.
    pub(crate) sockets: Measured,
}

/// Count this process's open descriptors/handles. Cheap and best-effort: any
/// error becomes a typed `Unavailable`, never a zero.
pub(crate) fn handle_census() -> HandleCensus {
    #[cfg(target_os = "linux")]
    {
        let entries = match std::fs::read_dir("/proc/self/fd") {
            Ok(rd) => rd,
            Err(_) => {
                return HandleCensus {
                    open: Measured::Unavailable("/proc/self/fd unreadable"),
                    sockets: Measured::Unavailable("/proc/self/fd unreadable"),
                }
            }
        };
        let paths: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
        // `read_dir` itself holds one descriptor open; it is closed by the time
        // a reader sees the number, so say so rather than silently over-count.
        let total = paths.len() as u64;
        if paths.len() > FD_CLASSIFY_CEILING {
            return HandleCensus {
                open: Measured::Counted(total),
                sockets: Measured::Unavailable(
                    "skipped: more open descriptors than the classification ceiling",
                ),
            };
        }
        let sockets = paths
            .iter()
            .filter(|p| {
                std::fs::read_link(p)
                    .map(|t| t.to_string_lossy().starts_with("socket:"))
                    .unwrap_or(false)
            })
            .count() as u64;
        HandleCensus {
            open: Measured::Counted(total),
            sockets: Measured::Counted(sockets),
        }
    }

    #[cfg(windows)]
    {
        // `GetProcessHandleCount` on the pseudo-handle: no OpenProcess, no
        // privileges, no allocation. It answers the resource-exhaustion
        // question (a leaked socket IS a leaked handle) even though it cannot
        // isolate the socket subset.
        let mut count: u32 = 0;
        let ok = unsafe {
            windows_sys::Win32::System::Threading::GetProcessHandleCount(
                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                &mut count as *mut u32,
            )
        };
        let open = if ok != 0 {
            Measured::Counted(u64::from(count))
        } else {
            Measured::Unavailable("GetProcessHandleCount failed")
        };
        HandleCensus {
            open,
            sockets: Measured::Unavailable(
                "windows: per-handle type classification needs an NtQuerySystemInformation \
                 handle-table walk — deliberately not done on a failure path",
            ),
        }
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        HandleCensus {
            open: Measured::Unavailable("no descriptor census on this platform"),
            sockets: Measured::Unavailable("no descriptor census on this platform"),
        }
    }
}

// ---------------------------------------------------------------------------
// Descriptor HEADROOM (plan 2026-09-09-the-pong-receive-path-has-no-liveness-
// signal-so-fd-exhaustion-still-reads-as-ui-death)
// ---------------------------------------------------------------------------
//
// [`handle_census`] measures a COUNT, and a count alone answers nothing about
// starvation: 900 open descriptors is idle under a 65,536 soft limit and fatal
// under 1,024. The question the UI-death verdict needs answered is "could this
// process have accepted the socket a `/ui-bridge/pong` arrives on?", and that
// is HEADROOM — the soft `RLIMIT_NOFILE` minus the open count.
//
// Two independent authorities for the same fact, per `verification-and-
// evidence` `a-control-must-test-the-property-it-names`:
//
// 1. the headroom read below — a direct measurement, taken on demand;
// 2. the EMFILE/ENFILE stamp (`util::fd_exhaustion::note_fd_exhaustion`, in
//    the LIB crate so the bin and lib copies of `process_helpers` stamp ONE
//    static) — a POSITIVE event
//    recorded by any site that actually hit the ceiling with the `io::Error`
//    in hand. It matters most exactly where (1) goes blind: a process with no
//    descriptor left cannot `read_dir("/proc/self/fd")` either, so at total
//    exhaustion the census is `Unavailable` — and its own failure is then
//    stamped through (2) rather than lost.

/// Count open descriptors WITHOUT classifying them. [`handle_census`] spends one
/// `read_link` per descriptor to find sockets; a headroom read needs only the
/// total and is taken on every heartbeat tick and every `/health`, so it does
/// not pay for the split.
pub(crate) fn open_descriptor_count() -> Measured {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_dir("/proc/self/fd") {
            Ok(rd) => Measured::Counted(rd.count() as u64),
            Err(e) => {
                // A census that could not open a directory because the
                // process is AT its descriptor ceiling is itself the positive
                // evidence — record it through the second authority instead
                // of letting it collapse into a bare "unreadable".
                if qontinui_runner_lib::util::fd_exhaustion::note_fd_exhaustion(&e) {
                    Measured::Unavailable("/proc/self/fd unreadable: descriptor ceiling reached")
                } else {
                    Measured::Unavailable("/proc/self/fd unreadable")
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // There is no `/proc/self/fd` to walk off Linux. On Windows the
        // census's `GetProcessHandleCount` counts every kernel handle —
        // threads, events, mutexes — not descriptors, with no per-process
        // ceiling to hold it against (see [`fd_soft_limit`]); on macOS and
        // other Unixes an RLIMIT exists but no descriptor count is taken.
        // Publishing a handle count as `fd_open` would mislabel it, so off
        // Linux this read is honestly UNKNOWN. The Windows handle count is
        // still reported by `handle_census` under its own name.
        Measured::Unavailable("descriptor count is measured on Linux only")
    }
}

/// The soft `RLIMIT_NOFILE` — the ceiling `open`/`accept`/`pipe` fail against
/// with `EMFILE`.
///
/// `Unavailable` — never `0` — when it cannot be read: `getrlimit` failed, the
/// limit is `RLIM_INFINITY` (no finite ceiling, so no headroom to measure), or
/// the platform has no such limit. A real soft limit of `0` is reported as
/// `Counted(0)`, because it is a measurement.
pub(crate) fn fd_soft_limit() -> Measured {
    #[cfg(unix)]
    {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `getrlimit` writes one `rlimit` through a valid pointer to a
        // stack local and has no other side effects.
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) };
        if rc != 0 {
            return Measured::Unavailable("getrlimit(RLIMIT_NOFILE) failed");
        }
        #[allow(clippy::unnecessary_cast)] // `rlim_t` is u64 on Linux, not everywhere.
        soft_limit_measurement(rl.rlim_cur as u64, libc::RLIM_INFINITY as u64)
    }

    #[cfg(windows)]
    {
        Measured::Unavailable(
            "windows: no per-process descriptor ceiling comparable to RLIMIT_NOFILE",
        )
    }

    #[cfg(not(any(unix, windows)))]
    {
        Measured::Unavailable("no descriptor limit on this platform")
    }
}

/// Map a raw soft limit onto a [`Measured`]. Split out so the
/// infinity-is-not-a-number rule is testable without a real `getrlimit`.
#[cfg_attr(not(unix), allow(dead_code))]
fn soft_limit_measurement(rlim_cur: u64, rlim_infinity: u64) -> Measured {
    if rlim_cur == rlim_infinity {
        Measured::Unavailable("RLIMIT_NOFILE is unlimited: no finite ceiling to measure against")
    } else {
        Measured::Counted(rlim_cur)
    }
}

/// Open-descriptor count held against the soft limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FdHeadroom {
    pub(crate) open: Measured,
    pub(crate) soft_limit: Measured,
}

impl FdHeadroom {
    /// `limit - open`, saturating (a limit lowered below the current count is
    /// zero headroom, not a wrap). `Unavailable` when EITHER side was not
    /// measured — a missing side is UNKNOWN, never a headroom of 0 or of the
    /// whole limit.
    pub(crate) fn headroom(&self) -> Measured {
        match (&self.open, &self.soft_limit) {
            (Measured::Counted(open), Measured::Counted(limit)) => {
                Measured::Counted(limit.saturating_sub(*open))
            }
            (Measured::Unavailable(why), _) | (_, Measured::Unavailable(why)) => {
                Measured::Unavailable(why)
            }
        }
    }
}

/// Read the current descriptor headroom. Best-effort; every failure is a typed
/// `Unavailable`.
pub(crate) fn fd_headroom() -> FdHeadroom {
    FdHeadroom {
        open: open_descriptor_count(),
        soft_limit: fd_soft_limit(),
    }
}

// ---------------------------------------------------------------------------
// The snapshot
// ---------------------------------------------------------------------------

/// The context block attached to a coord-egress failure — the WARN line and the
/// 502 body both carry this same object, so a log line and a caller's envelope
/// can be joined.
///
/// Shape (fields never removed, only added):
///
/// ```json
/// {
///   "client": "coord-mcp-proxy",
///   "process_uptime_ms": 41523118,          // or null
///   "open_handles": 412,                     // or {"unavailable": "…"}
///   "socket_handles": 88,                    // or {"unavailable": "…"}
///   "in_flight": 3,
///   "failures_total": 17,
///   "pool_introspection": "unavailable: …"
/// }
/// ```
pub(crate) fn snapshot(client: EgressClient) -> Value {
    let census = handle_census();
    json!({
        "client": client.as_str(),
        "process_uptime_ms": process_uptime_ms(),
        "open_handles": census.open.to_json(),
        "socket_handles": census.sockets.to_json(),
        "in_flight": IN_FLIGHT[client.index()].load(Ordering::Relaxed),
        "failures_total": FAILURES[client.index()].load(Ordering::Relaxed),
        "pool_introspection": POOL_INTROSPECTION,
    })
}

/// A one-line rendering of [`snapshot`] for a WARN that is already a string.
/// Deliberately terse: it is appended to an existing message.
pub(crate) fn snapshot_line(client: EgressClient) -> String {
    let census = handle_census();
    let uptime = match process_uptime_ms() {
        Some(ms) => ms.to_string(),
        None => "unknown".to_string(),
    };
    let m = |x: &Measured| match x {
        Measured::Counted(n) => n.to_string(),
        Measured::Unavailable(_) => "unavailable".to_string(),
    };
    format!(
        "egress[client={} uptime_ms={} open_handles={} socket_handles={} in_flight={} failures_total={}]",
        client.as_str(),
        uptime,
        m(&census.open),
        m(&census.sockets),
        IN_FLIGHT[client.index()].load(Ordering::Relaxed),
        FAILURES[client.index()].load(Ordering::Relaxed),
    )
}

/// The healthy-state baseline, emitted at INFO on [`BASELINE_INTERVAL`].
///
/// Without it a count captured mid-episode has nothing to be compared against,
/// and the process-age hypothesis stays untestable no matter how good the
/// failure-path block is. One line per interval carrying the process-wide
/// census plus every client's counters.
pub(crate) fn log_baseline() {
    let census = handle_census();
    let per_client: Vec<String> = EgressClient::ALL
        .iter()
        .map(|c| {
            format!(
                "{}={}/{}",
                c.as_str(),
                IN_FLIGHT[c.index()].load(Ordering::Relaxed),
                FAILURES[c.index()].load(Ordering::Relaxed)
            )
        })
        .collect();
    let m = |x: &Measured| match x {
        Measured::Counted(n) => n.to_string(),
        Measured::Unavailable(why) => format!("unavailable({why})"),
    };
    // Headroom against the soft limit. The two fields sit BEFORE the
    // variable-length `in_flight/failures` tail, not after it: a reader that
    // treats everything after `in_flight/failures` as per-client pairs would
    // otherwise swallow them. The count is the census's own, so the headroom
    // and `open_handles` on one line agree.
    let headroom = FdHeadroom {
        open: census.open.clone(),
        soft_limit: fd_soft_limit(),
    };
    info!(
        "egress_baseline: uptime_ms={} open_handles={} socket_handles={} fd_soft_limit={} \
         fd_headroom={} in_flight/failures {}",
        process_uptime_ms()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        m(&census.open),
        m(&census.sockets),
        m(&headroom.soft_limit),
        m(&headroom.headroom()),
        per_client.join(" "),
    );
}

/// Spawn the periodic baseline emitter. Idempotent — a second call is a no-op,
/// so a re-built router cannot end up with two tasks doubling the stream.
pub(crate) fn spawn_baseline_logger() {
    // No runtime (a unit test, a CLI bin building a router for inspection) ⇒
    // no baseline, rather than the panic `tokio::spawn` would raise. The claim
    // set stays honest either way: the baseline is a log stream, and its
    // absence is visible as an absence of lines.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    static SPAWNED: OnceLock<()> = OnceLock::new();
    if SPAWNED.set(()).is_err() {
        return;
    }
    tokio::spawn(async {
        // One immediately, so a short-lived process still leaves a baseline.
        log_baseline();
        loop {
            tokio::time::sleep(BASELINE_INTERVAL).await;
            log_baseline();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_indices_are_dense_and_unique() {
        let mut seen = [false; 7];
        for c in EgressClient::ALL {
            let i = c.index();
            assert!(i < IN_FLIGHT.len(), "{c:?} index {i} out of range");
            assert!(!seen[i], "{c:?} reuses index {i}");
            seen[i] = true;
        }
        assert!(seen.iter().all(|s| *s), "index space has a hole");
        assert_eq!(EgressClient::ALL.len(), IN_FLIGHT.len());
        assert_eq!(EgressClient::ALL.len(), FAILURES.len());
    }

    /// An unavailable count must never serialize as a number — a `0` here is a
    /// confidently-wrong measurement, which is the defect class this module
    /// exists to avoid, not one to reproduce.
    #[test]
    fn unavailable_never_renders_as_zero() {
        let j = Measured::Unavailable("no census on this platform").to_json();
        assert!(j.is_object(), "{j}");
        assert!(!j.is_number(), "{j}");
        assert_eq!(j["unavailable"], "no census on this platform");
        assert_eq!(Measured::Counted(0).to_json(), json!(0));
    }

    /// The snapshot's shape is a contract for `/coord-revive` and for anything
    /// grepping the 502 body — every field present, uptime honest about being
    /// unknown when nothing anchored it.
    #[test]
    fn snapshot_carries_every_field() {
        let v = snapshot(EgressClient::CoordMcpProxy);
        assert_eq!(v["client"], "coord-mcp-proxy");
        for k in [
            "process_uptime_ms",
            "open_handles",
            "socket_handles",
            "in_flight",
            "failures_total",
            "pool_introspection",
        ] {
            assert!(v.get(k).is_some(), "missing {k} in {v}");
        }
        assert!(
            v["pool_introspection"]
                .as_str()
                .unwrap_or_default()
                .starts_with("unavailable"),
            "reqwest exposes no pool counters and the field must say so: {v}"
        );
    }

    #[test]
    fn in_flight_guard_increments_and_decrements() {
        let c = EgressClient::VcsPr;
        let before = IN_FLIGHT[c.index()].load(Ordering::Relaxed);
        {
            let _g = in_flight(c);
            assert_eq!(IN_FLIGHT[c.index()].load(Ordering::Relaxed), before + 1);
        }
        assert_eq!(IN_FLIGHT[c.index()].load(Ordering::Relaxed), before);
    }

    #[test]
    fn record_failure_is_monotonic_per_client() {
        let c = EgressClient::DeviceJwtRefresher;
        let a = record_failure(c);
        let b = record_failure(c);
        assert_eq!(b, a + 1);
        // A different client's counter is untouched — the point of naming them.
        let other = FAILURES[EgressClient::CoordWrite.index()].load(Ordering::Relaxed);
        record_failure(c);
        assert_eq!(
            FAILURES[EgressClient::CoordWrite.index()].load(Ordering::Relaxed),
            other
        );
    }

    /// On Linux the census must actually answer — this is the platform the
    /// hypothesis will be tested on first, so a silently-unavailable count here
    /// would make the whole item inert.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_census_counts_real_descriptors() {
        let census = handle_census();
        let open = census
            .open
            .counted()
            .expect("Linux must count /proc/self/fd");
        assert!(open >= 3, "stdin/stdout/stderr at minimum, got {open}");
        // Sockets are a subset of open descriptors, never more.
        if let Some(s) = census.sockets.counted() {
            assert!(s <= open, "sockets {s} > open {open}");
        }
    }

    // ── descriptor headroom (plan 2026-09-09 pong receive path) ────────────

    /// An unlimited soft limit has no finite ceiling to measure headroom
    /// against, and must say so rather than becoming a number.
    #[test]
    fn unlimited_soft_limit_is_unavailable_not_a_number() {
        const INF: u64 = u64::MAX;
        assert!(matches!(
            soft_limit_measurement(INF, INF),
            Measured::Unavailable(_)
        ));
        assert_eq!(soft_limit_measurement(1024, INF), Measured::Counted(1024));
        // A real limit of 0 is a measurement, not an absence.
        assert_eq!(soft_limit_measurement(0, INF), Measured::Counted(0));
    }

    /// Headroom is UNKNOWN — never 0, never the whole limit — when either side
    /// could not be read.
    #[test]
    fn headroom_is_unavailable_rather_than_zero_when_either_side_is_unread() {
        let no_count = FdHeadroom {
            open: Measured::Unavailable("census failed"),
            soft_limit: Measured::Counted(1024),
        };
        assert_eq!(no_count.headroom(), Measured::Unavailable("census failed"));
        assert!(!no_count.headroom().to_json().is_number());

        let no_limit = FdHeadroom {
            open: Measured::Counted(900),
            soft_limit: Measured::Unavailable("no limit here"),
        };
        assert_eq!(no_limit.headroom(), Measured::Unavailable("no limit here"));
        assert_eq!(no_limit.headroom().counted(), None);

        let measured = FdHeadroom {
            open: Measured::Counted(1000),
            soft_limit: Measured::Counted(1024),
        };
        assert_eq!(measured.headroom(), Measured::Counted(24));

        // A limit lowered below the live count saturates to zero headroom.
        let over = FdHeadroom {
            open: Measured::Counted(2000),
            soft_limit: Measured::Counted(1024),
        };
        assert_eq!(over.headroom(), Measured::Counted(0));
    }

    /// Where the platform CAN answer, it must: on Linux the soft limit is
    /// always finite in practice for a test process, and the count is real.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_headroom_is_measured() {
        let h = fd_headroom();
        let open = h.open.counted().expect("Linux counts /proc/self/fd");
        assert!(open >= 3, "stdin/stdout/stderr at minimum, got {open}");
        if let Some(limit) = h.soft_limit.counted() {
            assert!(limit > 0, "a running test process has a positive limit");
            assert!(h.headroom().counted().is_some());
        }
    }

    /// Off Linux the descriptor count is UNKNOWN — never a kernel handle
    /// count relabelled as descriptors.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn descriptor_count_is_unavailable_off_linux() {
        assert!(matches!(open_descriptor_count(), Measured::Unavailable(_)));
    }

    /// Windows has no RLIMIT_NOFILE; the limit (and so the headroom) is typed
    /// UNKNOWN there, never 0.
    #[cfg(windows)]
    #[test]
    fn windows_soft_limit_is_unavailable() {
        assert!(matches!(fd_soft_limit(), Measured::Unavailable(_)));
        assert!(matches!(fd_headroom().headroom(), Measured::Unavailable(_)));
    }

    /// Uptime is `null`, not `0`, until something anchors the clock.
    #[test]
    fn uptime_is_unknown_until_anchored() {
        // Cannot un-set a OnceLock, so assert the WEAKER invariant that holds
        // in both orders: whatever it answers, it never lies by rendering an
        // un-anchored clock as a number.
        match process_uptime_ms() {
            None => assert_eq!(
                snapshot(EgressClient::CoordRead)["process_uptime_ms"],
                Value::Null
            ),
            Some(_) => assert!(
                snapshot(EgressClient::CoordRead)["process_uptime_ms"].is_number(),
                "an anchored clock must report a number"
            ),
        }
    }
}
