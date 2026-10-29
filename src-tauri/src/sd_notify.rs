//! systemd `sd_notify` integration for a runner run as a `Type=notify`
//! service with `WatchdogSec=` (plan
//! `2026-10-19-ci-nodes-run-the-tauri-runner-always-on-as-ci-node-role`,
//! Phase 2b).
//!
//! # What it sends, and from where
//!
//! - **`READY=1`**, once, when the HTTP API is bound and serving AND the
//!   ci_node `device_ci` coord subscription has connected. See
//!   [`ready_action`] for the two cases that send it without the
//!   subscription (the ci_node runtime is disabled on this box, or the
//!   subscription has not connected within [`CI_SUBSCRIPTION_READY_GRACE`]).
//!   While it waits on the subscription it first sends
//!   `EXTEND_TIMEOUT_USEC=` so the grace cannot collide with the unit's
//!   `TimeoutStartSec=` (systemd's default is also 90 s).
//! - **`WATCHDOG=1`**, from the EXISTING runtime-independent `wedge-watchdog`
//!   OS thread's tick (`health_monitor::watchdog_loop_with_hook`), never from
//!   the tokio-hosted self-probe loop. The decision is [`watchdog_decision`]:
//!   keep alive while the backend answers `/livez` and the probe thread's
//!   heartbeat is fresh; while it does NOT, keep alive only when restarting
//!   would destroy hosted work — the readiness gate below.
//!
//! # The readiness gate (served policy `production-and-cost` `runner-lifecycle`)
//!
//! A watchdog kill of a wedged runner is a stop of a running runner, which
//! the policy forbids while the runner hosts work. So a withheld keepalive
//! (which lets systemd kill and restart the process) happens only when
//! nothing says the runner hosts work:
//!
//! 1. **In-flight CI builds** — the ci_node runtime holds a
//!    [`CiBuildInFlight`] guard for every running dispatch; a non-zero count
//!    keeps the runner alive. `/restart-readiness` counts `claude` processes
//!    and session rows only, so it cannot see these.
//! 2. **The last-good readiness snapshot** — a `/restart-readiness` verdict
//!    computed by the SAME sync
//!    [`build_verdict`](crate::mcp::restart_readiness::build_verdict),
//!    published by the async side into a std cell this module owns: every
//!    background census pass (`tracking_health::run_once`,
//!    [`publish_census_report`]) and every `GET /restart-readiness`
//!    ([`publish_readiness_verdict`]). The most recent publish wins; it is
//!    stamped with a monotonic [`Instant`], so ordering and age are immune to
//!    a wall-clock step.
//!
//! The watchdog thread never calls the HTTP API and never runs a census: both
//! depend on the tokio runtime a wedge starves. It reads an atomic and
//! `try_lock`s the cell — a contended cell reads UNKNOWN and keeps alive, so
//! the watchdog thread can never block on it.
//!
//! Semantics: any session counted in the snapshot (an unsafe verdict, which
//! includes every UNKNOWN the verdict itself fails closed on) keeps the
//! runner alive; an empty (safe) snapshot withholds; a census that has never
//! completed, or a snapshot older than [`SNAPSHOT_MAX_AGE`], is UNKNOWN and
//! keeps alive (fail closed).
//!
//! **Known limit, stated rather than hidden:** the background census runs
//! every `tracking_health::CHECK_INTERVAL` (600 s), so a session opened after
//! the last pass and before a wedge is not in the snapshot unless a
//! `/restart-readiness` read published a fresher verdict. A ci_node runner
//! hosts no AUTONOMOUS agent sessions (runner#2289 refuses them), but an
//! operator can still open a terminal on it, and one opened inside that
//! window is not protected.
//!
//! # Inert everywhere else
//!
//! Every send is a no-op when `NOTIFY_SOCKET` is unset — desktop runners,
//! dev runners, temp runners, tests. On a non-Linux build `NOTIFY_SOCKET` is
//! never read at all, so nothing is decided, sent or logged. The decision
//! logic is platform-independent so it is unit-tested on every OS.
//!
//! The transport is a direct, NON-BLOCKING datagram write (no crate):
//! `NOTIFY_SOCKET` is a filesystem path, or an abstract-namespace name
//! spelled with a leading `@`. A full socket buffer is a missed tick, never a
//! blocked watchdog thread.

use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use crate::mcp::restart_readiness::{self, RestartReadiness};
use crate::mcp::session_work_status::{self, SessionStatusSource};
use crate::session::tracking_health::{self, TrackingHealthReport};

/// The environment variable systemd sets for a `Type=notify` service.
pub const NOTIFY_SOCKET_ENV: &str = "NOTIFY_SOCKET";

/// How long after the HTTP API is serving `READY=1` waits for the `device_ci`
/// subscription before it is sent anyway.
///
/// Why not wait forever: the subscription depends on coord being reachable,
/// and a coord outage at boot must not keep a runner whose local API is fine
/// out of `active`. The subscription reconnects on its own back-off after
/// READY. The wait is bounded against `TimeoutStartSec=` by
/// [`READY_WAIT_EXTENSION`], sent before it starts.
pub const CI_SUBSCRIPTION_READY_GRACE: Duration = Duration::from_secs(90);

/// The `EXTEND_TIMEOUT_USEC=` sent when the API is serving and READY is still
/// waiting on the subscription: the grace, plus margin for the up-to-one
/// watchdog tick it can take for the grace arm to fire. systemd extends the
/// start timeout to at least this far from receipt, so the default
/// `TimeoutStartSec=90s` cannot kill a runner mid-grace and turn one coord
/// outage into a `Restart=always` start-limit loop.
pub const READY_WAIT_EXTENSION: Duration = Duration::from_secs(150);

/// A published readiness snapshot older than this is UNKNOWN (keep alive):
/// twice the background census interval, so one missed pass is tolerated
/// and a census task that has died is not trusted forever.
pub const SNAPSHOT_MAX_AGE: Duration =
    Duration::from_secs(tracking_health::CHECK_INTERVAL.as_secs() * 2);

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// The `NOTIFY_SOCKET` value, read once. `None` (unset or empty) makes every
/// send a no-op.
#[cfg(target_os = "linux")]
fn notify_socket() -> Option<&'static std::ffi::OsString> {
    static SOCKET: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
    SOCKET
        .get_or_init(|| std::env::var_os(NOTIFY_SOCKET_ENV).filter(|s| !s.is_empty()))
        .as_ref()
}

/// Non-Linux builds have no service manager to notify: never read the
/// variable, so no READY/WITHHOLDING decision is made or logged.
#[cfg(not(target_os = "linux"))]
fn notify_socket() -> Option<&'static std::ffi::OsString> {
    None
}

/// Send `state` to the service manager. `Ok(false)` when there is no
/// `NOTIFY_SOCKET` (always, on a non-Linux build); `Ok(true)` when the
/// datagram was written.
pub fn notify(state: &str) -> std::io::Result<bool> {
    match notify_socket() {
        Some(socket) => send_to(socket, state),
        None => Ok(false),
    }
}

/// Write one datagram to `socket` — a path, or `@name` for the abstract
/// namespace. Non-blocking: a full receive buffer returns `WouldBlock`, which
/// every caller treats as a missed tick. The seam the Linux transport test
/// drives.
#[cfg(target_os = "linux")]
pub fn send_to(socket: &OsStr, state: &str) -> std::io::Result<bool> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};

    let bytes = socket.as_bytes();
    let sock = UnixDatagram::unbound()?;
    sock.set_nonblocking(true)?;
    let sent = match bytes.strip_prefix(b"@") {
        Some(name) => {
            sock.send_to_addr(state.as_bytes(), &SocketAddr::from_abstract_name(name)?)?
        }
        None => sock.send_to(state.as_bytes(), std::path::Path::new(socket))?,
    };
    if sent != state.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            format!(
                "sd_notify: short datagram write ({sent} of {} bytes)",
                state.len()
            ),
        ));
    }
    Ok(true)
}

/// Non-Linux builds: systemd does not exist, so nothing is ever sent.
#[cfg(not(target_os = "linux"))]
pub fn send_to(_socket: &OsStr, _state: &str) -> std::io::Result<bool> {
    Ok(false)
}

// ---------------------------------------------------------------------------
// WATCHDOG_USEC — the interval systemd expects keepalives within
// ---------------------------------------------------------------------------

/// Parse `WATCHDOG_USEC` / `WATCHDOG_PID` as `sd_watchdog_enabled(3)` does:
/// `None` when unset, unparseable, zero, or addressed to another pid.
pub fn parse_watchdog_usec(
    usec: Option<&str>,
    pid: Option<&str>,
    own_pid: u32,
) -> Option<Duration> {
    if let Some(pid) = pid {
        if pid.trim().parse::<u32>().ok()? != own_pid {
            return None;
        }
    }
    let usec: u64 = usec?.trim().parse().ok()?;
    (usec > 0).then(|| Duration::from_micros(usec))
}

/// This process's watchdog interval, if systemd armed one for it. Never read
/// off Linux.
pub fn watchdog_interval() -> Option<Duration> {
    if cfg!(not(target_os = "linux")) {
        return None;
    }
    parse_watchdog_usec(
        std::env::var("WATCHDOG_USEC").ok().as_deref(),
        std::env::var("WATCHDOG_PID").ok().as_deref(),
        std::process::id(),
    )
}

/// The wedge-watchdog tick to use given systemd's interval. A tick longer
/// than half the interval leaves too little slack for one late wake, so it is
/// shortened to a third of the interval (floored at 1 s). Returns the tick
/// and whether it was shortened.
pub fn tick_for_watchdog(default: Duration, interval: Option<Duration>) -> (Duration, bool) {
    match interval {
        Some(iv) if default > iv / 2 => ((iv / 3).max(Duration::from_secs(1)), true),
        _ => (default, false),
    }
}

// ---------------------------------------------------------------------------
// In-flight CI builds
// ---------------------------------------------------------------------------

static CI_BUILDS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// RAII marker for one running ci_node dispatch. Created when the build's slot
/// is taken and moved into the executor task, so the count is released on
/// completion, cancellation, an early return, a panic, or the task being
/// dropped — every path out.
#[derive(Debug)]
pub struct CiBuildInFlight(&'static AtomicUsize);

impl CiBuildInFlight {
    fn on(counter: &'static AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        CiBuildInFlight(counter)
    }
}

impl Drop for CiBuildInFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Mark one ci_node dispatch as running until the guard drops.
pub fn ci_build_in_flight() -> CiBuildInFlight {
    CiBuildInFlight::on(&CI_BUILDS_IN_FLIGHT)
}

/// How many ci_node dispatches are running. Lock-free.
pub fn ci_builds_in_flight() -> usize {
    CI_BUILDS_IN_FLIGHT.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// READY=1
// ---------------------------------------------------------------------------

/// State of the ci_node `device_ci` subscription, as READY reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiSubscription {
    /// The ci_node runtime is starting, or the socket has not connected yet.
    Pending,
    /// The `device_ci` socket connected at least once.
    Up,
    /// The ci_node runtime is disabled on this box (no device id, or an
    /// isolated runner with no coord) — there is no subscription to wait for.
    NotApplicable,
}

impl CiSubscription {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => CiSubscription::Up,
            2 => CiSubscription::NotApplicable,
            _ => CiSubscription::Pending,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            CiSubscription::Pending => 0,
            CiSubscription::Up => 1,
            CiSubscription::NotApplicable => 2,
        }
    }
}

/// Why `READY=1` is being sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyReason {
    /// The HTTP API is serving and the `device_ci` subscription is up.
    AllUp,
    /// The HTTP API is serving and the ci_node runtime is disabled here.
    CiNotApplicable,
    /// The HTTP API is serving and the subscription did not connect within
    /// [`CI_SUBSCRIPTION_READY_GRACE`].
    CiGraceElapsed,
}

impl ReadyReason {
    fn status(self) -> &'static str {
        match self {
            ReadyReason::AllUp => "HTTP API serving; ci_node device_ci subscription up",
            ReadyReason::CiNotApplicable => {
                "HTTP API serving; ci_node runtime disabled on this runner"
            }
            ReadyReason::CiGraceElapsed => {
                "HTTP API serving; ci_node device_ci subscription NOT yet connected \
                 (grace elapsed, it keeps reconnecting)"
            }
        }
    }
}

/// What the READY latch should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyAction {
    /// Nothing to send yet.
    Wait,
    /// The API is serving and READY waits on the subscription: extend the
    /// start timeout first.
    ExtendStartTimeout,
    Ready(ReadyReason),
}

/// Pure READY decision. `serving_for` is how long the HTTP API has been
/// serving (`None` = not serving yet); `extended` is whether the start-timeout
/// extension was already sent.
pub fn ready_action(
    serving_for: Option<Duration>,
    ci: CiSubscription,
    extended: bool,
) -> ReadyAction {
    let Some(serving_for) = serving_for else {
        return ReadyAction::Wait;
    };
    match ci {
        CiSubscription::Up => ReadyAction::Ready(ReadyReason::AllUp),
        CiSubscription::NotApplicable => ReadyAction::Ready(ReadyReason::CiNotApplicable),
        CiSubscription::Pending if serving_for >= CI_SUBSCRIPTION_READY_GRACE => {
            ReadyAction::Ready(ReadyReason::CiGraceElapsed)
        }
        CiSubscription::Pending if !extended => ReadyAction::ExtendStartTimeout,
        CiSubscription::Pending => ReadyAction::Wait,
    }
}

/// The READY latch's two once-flags.
#[derive(Debug, Default)]
pub struct ReadyLatch {
    ready_sent: AtomicBool,
    extend_sent: AtomicBool,
}

impl ReadyLatch {
    const fn new() -> Self {
        ReadyLatch {
            ready_sent: AtomicBool::new(false),
            extend_sent: AtomicBool::new(false),
        }
    }
}

static HTTP_SERVING_SINCE: OnceLock<Instant> = OnceLock::new();
static CI_SUBSCRIPTION: AtomicU8 = AtomicU8::new(0);
static READY_LATCH: ReadyLatch = ReadyLatch::new();

/// The HTTP API has bound its listener and is serving. Called once from
/// `mcp_api::start_server`.
pub fn mark_http_serving() {
    let _ = HTTP_SERVING_SINCE.set(Instant::now());
    poll_ready();
}

/// The ci_node `device_ci` socket connected.
pub fn mark_ci_subscription_up() {
    CI_SUBSCRIPTION.store(CiSubscription::Up.as_u8(), Ordering::SeqCst);
    poll_ready();
}

/// The ci_node runtime will not run on this runner.
pub fn mark_ci_subscription_not_applicable() {
    // Never downgrade an `Up` (cannot happen today; cheap to guard).
    let _ = CI_SUBSCRIPTION.compare_exchange(
        CiSubscription::Pending.as_u8(),
        CiSubscription::NotApplicable.as_u8(),
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
    poll_ready();
}

/// Send `READY=1` (or the start-timeout extension) if due. Called from the
/// mark sites and from every watchdog tick (which is what fires the grace
/// arm).
pub fn poll_ready() {
    if notify_socket().is_none() {
        return;
    }
    let serving_for = HTTP_SERVING_SINCE.get().map(|t| t.elapsed());
    let ci = CiSubscription::from_u8(CI_SUBSCRIPTION.load(Ordering::SeqCst));
    poll_ready_with(&READY_LATCH, serving_for, ci, notify);
}

/// [`poll_ready`] over an explicit latch and sender — the test seam. Returns
/// the action it attempted.
pub fn poll_ready_with<F>(
    latch: &ReadyLatch,
    serving_for: Option<Duration>,
    ci: CiSubscription,
    send: F,
) -> ReadyAction
where
    F: FnOnce(&str) -> std::io::Result<bool>,
{
    if latch.ready_sent.load(Ordering::SeqCst) {
        return ReadyAction::Wait;
    }
    let action = ready_action(serving_for, ci, latch.extend_sent.load(Ordering::SeqCst));
    match action {
        ReadyAction::Wait => {}
        ReadyAction::ExtendStartTimeout => {
            if latch
                .extend_sent
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let msg = format!(
                    "EXTEND_TIMEOUT_USEC={}\nSTATUS=HTTP API serving; waiting up to {}s for the \
                     ci_node device_ci subscription before READY",
                    READY_WAIT_EXTENSION.as_micros(),
                    CI_SUBSCRIPTION_READY_GRACE.as_secs()
                );
                if let Err(e) = send(&msg) {
                    latch.extend_sent.store(false, Ordering::SeqCst);
                    tracing::warn!("sd_notify: EXTEND_TIMEOUT_USEC send failed ({e}); will retry");
                }
            }
        }
        ReadyAction::Ready(reason) => {
            if latch
                .ready_sent
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                match send(&format!("READY=1\nSTATUS={}", reason.status())) {
                    Ok(_) => tracing::info!("sd_notify: READY=1 sent ({})", reason.status()),
                    Err(e) => {
                        // Let a later poll retry rather than leaving the unit
                        // activating.
                        latch.ready_sent.store(false, Ordering::SeqCst);
                        tracing::warn!("sd_notify: READY=1 send failed ({e}); will retry");
                    }
                }
            }
        }
    }
    action
}

// ---------------------------------------------------------------------------
// The last-good readiness snapshot
// ---------------------------------------------------------------------------

/// One published readiness verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedReadiness {
    /// Monotonic stamp of the publish — the only clock used for ordering and
    /// age.
    pub stamp: Instant,
    pub safe_to_restart: bool,
    pub reason: String,
    /// `"census"` or `"restart-readiness"`.
    pub source: &'static str,
}

/// What the watchdog thread sees of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessView {
    /// No verdict has ever been published — the census has not completed.
    NeverRan,
    /// The cell was contended this tick.
    Unreadable,
    /// The newest verdict is older than [`SNAPSHOT_MAX_AGE`].
    Stale {
        age: Duration,
    },
    Published(PublishedReadiness),
}

static LAST_READINESS: Mutex<Option<PublishedReadiness>> = Mutex::new(None);

/// Store `p` into `cell` unless the cell already holds a NEWER stamp.
fn publish_into(cell: &Mutex<Option<PublishedReadiness>>, p: PublishedReadiness) {
    let mut g = cell.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|cur| p.stamp >= cur.stamp) {
        *g = Some(p);
    }
}

/// Read `cell` at monotonic `now` without ever blocking.
fn read_from(cell: &Mutex<Option<PublishedReadiness>>, now: Instant) -> ReadinessView {
    let g = match cell.try_lock() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return ReadinessView::Unreadable,
    };
    match g.as_ref() {
        None => ReadinessView::NeverRan,
        Some(p) => {
            let age = now.saturating_duration_since(p.stamp);
            if age > SNAPSHOT_MAX_AGE {
                ReadinessView::Stale { age }
            } else {
                ReadinessView::Published(p.clone())
            }
        }
    }
}

/// The verdict `build_verdict` reaches over ONE background census report.
///
/// The background census never consults coord, so the work axis is reported
/// degraded (every live process blocks — the fail-closed count). The AI plane
/// is the census's own view of that plane's processes, which
/// `build_verdict` counts through `live_claude.ai_plane`. Drain state is
/// reported but never feeds `safe_to_restart`.
pub fn census_verdict(report: &TrackingHealthReport, now_ms: i64) -> RestartReadiness {
    restart_readiness::build_verdict(
        Some(restart_readiness::terminal_plane_from(report, &[], now_ms)),
        Some(restart_readiness::headless_plane_from(report)),
        Some(restart_readiness::holder_plane_from(report)),
        Some(restart_readiness::ai_plane_from(
            &[],
            &report.ai_plane,
            now_ms,
        )),
        Some(restart_readiness::live_claude_totals_from(report)),
        Vec::new(),
        restart_readiness::DrainInfo {
            already_drained: crate::drain::already_drained(),
            is_draining: crate::drain::is_draining(),
            would_be_noop: true,
            covers: restart_readiness::DRAIN_COVERS,
            call: "POST /drain".to_string(),
            coord_drain: crate::coord_drain_state::snapshot(),
        },
        restart_readiness::census_info(Some(report), now_ms),
        SessionStatusSource {
            source: "unavailable",
            door: session_work_status::DOOR,
            requested: 0,
            resolved: 0,
            degraded: true,
            note: "the background census does not consult coord; every live process blocks"
                .to_string(),
        },
    )
}

/// Publish the verdict over a completed background census pass. Called by
/// `tracking_health::run_once` on the async side.
pub fn publish_census_report(report: &TrackingHealthReport) {
    let v = census_verdict(report, chrono::Utc::now().timestamp_millis());
    publish_into(
        &LAST_READINESS,
        PublishedReadiness {
            stamp: Instant::now(),
            safe_to_restart: v.safe_to_restart,
            reason: v.reason,
            source: "census",
        },
    );
}

/// Publish the verdict a `GET /restart-readiness` just served.
pub fn publish_readiness_verdict(v: &RestartReadiness) {
    publish_into(
        &LAST_READINESS,
        PublishedReadiness {
            stamp: Instant::now(),
            safe_to_restart: v.safe_to_restart,
            reason: v.reason.clone(),
            source: "restart-readiness",
        },
    );
}

/// The snapshot as the watchdog thread reads it. Never blocks.
pub fn readiness_view() -> ReadinessView {
    read_from(&LAST_READINESS, Instant::now())
}

// ---------------------------------------------------------------------------
// WATCHDOG=1
// ---------------------------------------------------------------------------

/// Why a keepalive is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveReason {
    /// The backend answers `/livez` and the probe heartbeat is fresh.
    Healthy,
    /// Wedged, but a CI build is running or the snapshot counts live work.
    HostingWork,
    /// Wedged, and the snapshot is UNKNOWN (never ran, unreadable, or stale).
    ReadinessUnknown,
}

/// One watchdog decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogDecision {
    Keepalive(KeepaliveReason),
    /// Wedged, no CI build running, and the snapshot says restart is safe:
    /// send nothing, so systemd's `WatchdogSec=` expires and it restarts the
    /// process.
    Withhold,
}

/// Pure keepalive decision.
///
/// `backend_wedged` is `health_monitor::backend_wedged()`; `monitor_fresh` is
/// whether the self-probe thread's heartbeat is within
/// `WATCHDOG_MONITOR_STALL_SECS` (a stalled probe thread's `backend_wedged`
/// is stale, so it counts as not healthy); `ci_builds` is
/// [`ci_builds_in_flight`].
pub fn watchdog_decision(
    backend_wedged: bool,
    monitor_fresh: bool,
    ci_builds: usize,
    readiness: &ReadinessView,
) -> WatchdogDecision {
    if !backend_wedged && monitor_fresh {
        return WatchdogDecision::Keepalive(KeepaliveReason::Healthy);
    }
    if ci_builds > 0 {
        return WatchdogDecision::Keepalive(KeepaliveReason::HostingWork);
    }
    match readiness {
        ReadinessView::NeverRan | ReadinessView::Unreadable | ReadinessView::Stale { .. } => {
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        }
        ReadinessView::Published(p) if !p.safe_to_restart => {
            WatchdogDecision::Keepalive(KeepaliveReason::HostingWork)
        }
        ReadinessView::Published(_) => WatchdogDecision::Withhold,
    }
}

/// The datagram for a decision, or `None` when nothing is sent.
pub fn watchdog_message(
    decision: WatchdogDecision,
    ci_builds: usize,
    readiness: &ReadinessView,
) -> Option<String> {
    let reason = if ci_builds > 0 {
        format!("{ci_builds} ci_node build(s) in flight")
    } else {
        match readiness {
            ReadinessView::Published(p) => p.reason.clone(),
            ReadinessView::NeverRan => "no readiness census has completed yet".to_string(),
            ReadinessView::Unreadable => {
                "the readiness snapshot was contended this tick".to_string()
            }
            ReadinessView::Stale { age } => {
                format!("the newest readiness snapshot is {}s old", age.as_secs())
            }
        }
    };
    match decision {
        WatchdogDecision::Keepalive(KeepaliveReason::Healthy) => Some("WATCHDOG=1".to_string()),
        WatchdogDecision::Keepalive(KeepaliveReason::HostingWork) => Some(format!(
            "WATCHDOG=1\nSTATUS=wedged; restart withheld, runner hosts work: {reason}"
        )),
        WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown) => Some(format!(
            "WATCHDOG=1\nSTATUS=wedged; restart withheld, readiness UNKNOWN: {reason}"
        )),
        WatchdogDecision::Withhold => None,
    }
}

/// One watchdog-thread tick: maybe send `READY=1`, then decide and send the
/// keepalive. Returns the decision for the caller's incident record, or
/// `None` when there is no `NOTIFY_SOCKET` (nothing decided, nothing sent).
pub fn watchdog_tick(backend_wedged: bool, monitor_fresh: bool) -> Option<WatchdogDecision> {
    notify_socket()?;
    poll_ready();
    let readiness = readiness_view();
    let ci_builds = ci_builds_in_flight();
    let decision = watchdog_decision(backend_wedged, monitor_fresh, ci_builds, &readiness);
    if let Some(msg) = watchdog_message(decision, ci_builds, &readiness) {
        // Best-effort: a failed or `WouldBlock` send is a missed tick, which
        // the service manager already tolerates up to `WatchdogSec=`.
        let _ = notify(&msg);
    }
    Some(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(safe: bool) -> ReadinessView {
        ReadinessView::Published(PublishedReadiness {
            stamp: Instant::now(),
            safe_to_restart: safe,
            reason: if safe {
                "no live agent sessions in any plane".into()
            } else {
                "1 live session".into()
            },
            source: "census",
        })
    }

    fn entry(stamp: Instant, safe: bool) -> PublishedReadiness {
        PublishedReadiness {
            stamp,
            safe_to_restart: safe,
            reason: String::new(),
            source: "census",
        }
    }

    fn empty_report(now_ms: i64) -> TrackingHealthReport {
        TrackingHealthReport {
            checked_at_ms: now_ms,
            live_claude_total: 0,
            tracked_open_total: 0,
            terminal_hosted: vec![],
            ai_plane: vec![],
            headless_exempt: vec![],
            live_untracked: vec![],
            tracked_dead: vec![],
            holder_hosted: vec![],
            holder_panes: vec![],
            holder_census_error: None,
        }
    }

    fn live_process(pid: u32) -> crate::session::tracking_health::LiveClaudeProcess {
        crate::session::tracking_health::LiveClaudeProcess {
            pid,
            parent_pid: Some(1),
            image: Some("claude".to_string()),
            age_s: Some(90),
            cwd: None,
            has_live_children: Some(false),
            nested_under_claude: false,
            session_id: None,
            session_status: None,
            blocks_restart: true,
            wind_down: None,
        }
    }

    #[test]
    fn healthy_keeps_alive_whatever_the_snapshot() {
        for view in [
            ReadinessView::NeverRan,
            ReadinessView::Unreadable,
            ReadinessView::Stale {
                age: Duration::from_secs(5_000),
            },
            published(true),
            published(false),
        ] {
            assert_eq!(
                watchdog_decision(false, true, 0, &view),
                WatchdogDecision::Keepalive(KeepaliveReason::Healthy)
            );
        }
    }

    #[test]
    fn wedged_and_safe_withholds_the_keepalive() {
        let view = published(true);
        assert_eq!(
            watchdog_decision(true, true, 0, &view),
            WatchdogDecision::Withhold
        );
        assert_eq!(watchdog_message(WatchdogDecision::Withhold, 0, &view), None);
    }

    #[test]
    fn wedged_and_unsafe_keeps_alive() {
        let view = published(false);
        let d = watchdog_decision(true, true, 0, &view);
        assert_eq!(d, WatchdogDecision::Keepalive(KeepaliveReason::HostingWork));
        let msg = watchdog_message(d, 0, &view).unwrap();
        assert!(msg.starts_with("WATCHDOG=1\n"), "{msg}");
    }

    #[test]
    fn a_stalled_probe_thread_is_not_healthy() {
        assert_eq!(
            watchdog_decision(false, false, 0, &published(true)),
            WatchdogDecision::Withhold
        );
        assert_eq!(
            watchdog_decision(false, false, 0, &published(false)),
            WatchdogDecision::Keepalive(KeepaliveReason::HostingWork)
        );
    }

    #[test]
    fn wedged_with_a_ci_build_in_flight_keeps_alive_over_a_safe_snapshot() {
        let view = published(true);
        let d = watchdog_decision(true, true, 1, &view);
        assert_eq!(d, WatchdogDecision::Keepalive(KeepaliveReason::HostingWork));
        let msg = watchdog_message(d, 1, &view).unwrap();
        assert!(msg.contains("1 ci_node build(s) in flight"), "{msg}");
    }

    #[test]
    fn ci_build_guard_counts_and_releases_on_every_exit() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        {
            let _a = CiBuildInFlight::on(&COUNTER);
            let _b = CiBuildInFlight::on(&COUNTER);
            assert_eq!(COUNTER.load(Ordering::SeqCst), 2);
        }
        assert_eq!(COUNTER.load(Ordering::SeqCst), 0);
        // A panicking build releases its count during unwind.
        let r = std::panic::catch_unwind(|| {
            let _g = CiBuildInFlight::on(&COUNTER);
            assert_eq!(COUNTER.load(Ordering::SeqCst), 1);
            panic!("build panicked");
        });
        assert!(r.is_err());
        assert_eq!(COUNTER.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn never_ran_census_is_unknown_and_keeps_alive() {
        let cell = Mutex::new(None);
        let view = read_from(&cell, Instant::now());
        assert_eq!(view, ReadinessView::NeverRan);
        assert_eq!(
            watchdog_decision(true, true, 0, &view),
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        );
    }

    #[test]
    fn contended_snapshot_is_unknown_and_keeps_alive() {
        let cell = Mutex::new(None);
        let _held = cell.lock().unwrap();
        let view = read_from(&cell, Instant::now());
        assert_eq!(view, ReadinessView::Unreadable);
        assert_eq!(
            watchdog_decision(true, true, 0, &view),
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        );
    }

    #[test]
    fn a_stale_safe_snapshot_is_unknown_and_keeps_alive() {
        let cell = Mutex::new(None);
        let stamp = Instant::now();
        publish_into(&cell, entry(stamp, true));
        // Within the bound it is trusted...
        let fresh = read_from(&cell, stamp + SNAPSHOT_MAX_AGE);
        assert_eq!(
            watchdog_decision(true, true, 0, &fresh),
            WatchdogDecision::Withhold
        );
        // ...past it, it is UNKNOWN.
        let late = stamp + SNAPSHOT_MAX_AGE + Duration::from_secs(1);
        let view = read_from(&cell, late);
        assert!(matches!(view, ReadinessView::Stale { .. }), "{view:?}");
        assert_eq!(
            watchdog_decision(true, true, 0, &view),
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        );
    }

    #[test]
    fn snapshot_empty_withholds() {
        let now = 1_800_000_000_000;
        let v = census_verdict(&empty_report(now), now);
        assert!(v.safe_to_restart, "{}", v.reason);
        let cell = Mutex::new(None);
        let stamp = Instant::now();
        publish_into(&cell, entry(stamp, v.safe_to_restart));
        assert_eq!(
            watchdog_decision(true, true, 0, &read_from(&cell, stamp)),
            WatchdogDecision::Withhold
        );
    }

    #[test]
    fn snapshot_with_session_keeps_alive() {
        let now = 1_800_000_000_000;
        let mut report = empty_report(now);
        report.terminal_hosted = vec![live_process(4242)];
        report.live_claude_total = 1;
        let v = census_verdict(&report, now);
        assert!(!v.safe_to_restart, "{}", v.reason);

        let mut ai_report = empty_report(now);
        ai_report.ai_plane = vec![live_process(4343)];
        ai_report.live_claude_total = 1;
        assert!(!census_verdict(&ai_report, now).safe_to_restart);

        let cell = Mutex::new(None);
        let stamp = Instant::now();
        publish_into(&cell, entry(stamp, v.safe_to_restart));
        assert_eq!(
            watchdog_decision(true, true, 0, &read_from(&cell, stamp)),
            WatchdogDecision::Keepalive(KeepaliveReason::HostingWork)
        );
    }

    #[test]
    fn an_older_stamp_never_replaces_a_newer_one() {
        let cell = Mutex::new(None);
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_secs(10);
        let t2 = t0 + Duration::from_secs(20);
        publish_into(&cell, entry(t1, false));
        publish_into(&cell, entry(t0, true));
        assert_eq!(
            read_from(&cell, t1),
            ReadinessView::Published(entry(t1, false))
        );
        publish_into(&cell, entry(t2, true));
        assert_eq!(
            read_from(&cell, t2),
            ReadinessView::Published(entry(t2, true))
        );
    }

    #[test]
    fn ready_extends_the_start_timeout_then_waits_within_grace() {
        let short = Some(Duration::from_secs(1));
        assert_eq!(
            ready_action(None, CiSubscription::Up, false),
            ReadyAction::Wait
        );
        assert_eq!(
            ready_action(short, CiSubscription::Pending, false),
            ReadyAction::ExtendStartTimeout
        );
        assert_eq!(
            ready_action(short, CiSubscription::Pending, true),
            ReadyAction::Wait
        );
        assert_eq!(
            ready_action(short, CiSubscription::Up, false),
            ReadyAction::Ready(ReadyReason::AllUp)
        );
        assert_eq!(
            ready_action(short, CiSubscription::NotApplicable, false),
            ReadyAction::Ready(ReadyReason::CiNotApplicable)
        );
        assert_eq!(
            ready_action(
                Some(CI_SUBSCRIPTION_READY_GRACE),
                CiSubscription::Pending,
                true
            ),
            ReadyAction::Ready(ReadyReason::CiGraceElapsed)
        );
        assert!(READY_WAIT_EXTENSION > CI_SUBSCRIPTION_READY_GRACE);
    }

    #[test]
    fn poll_ready_sends_the_extension_once_and_retries_a_failed_ready() {
        let latch = ReadyLatch::default();
        let short = Some(Duration::from_secs(1));
        let mut sent: Vec<String> = Vec::new();

        poll_ready_with(&latch, short, CiSubscription::Pending, |m| {
            sent.push(m.to_string());
            Ok(true)
        });
        poll_ready_with(&latch, short, CiSubscription::Pending, |m| {
            sent.push(m.to_string());
            Ok(true)
        });
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].starts_with("EXTEND_TIMEOUT_USEC=150000000\n"),
            "{sent:?}"
        );

        // A failed READY send resets the latch...
        poll_ready_with(&latch, short, CiSubscription::Up, |_| {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        });
        assert!(!latch.ready_sent.load(Ordering::SeqCst));
        // ...so the next poll sends it, and nothing after that.
        poll_ready_with(&latch, short, CiSubscription::Up, |m| {
            sent.push(m.to_string());
            Ok(true)
        });
        assert!(sent[1].starts_with("READY=1\n"), "{sent:?}");
        assert_eq!(
            poll_ready_with(&latch, short, CiSubscription::Up, |m| {
                sent.push(m.to_string());
                Ok(true)
            }),
            ReadyAction::Wait
        );
        assert_eq!(sent.len(), 2, "{sent:?}");
    }

    #[test]
    fn watchdog_usec_is_parsed_like_sd_watchdog_enabled() {
        let own = 4242;
        assert_eq!(
            parse_watchdog_usec(Some("50000000"), None, own),
            Some(Duration::from_secs(50))
        );
        assert_eq!(
            parse_watchdog_usec(Some("50000000"), Some("4242"), own),
            Some(Duration::from_secs(50))
        );
        assert_eq!(parse_watchdog_usec(Some("50000000"), Some("1"), own), None);
        assert_eq!(parse_watchdog_usec(Some("0"), None, own), None);
        assert_eq!(parse_watchdog_usec(Some("nope"), None, own), None);
        assert_eq!(parse_watchdog_usec(None, None, own), None);
    }

    #[test]
    fn the_tick_is_shortened_when_it_exceeds_half_the_watchdog_interval() {
        let tick = Duration::from_secs(15);
        assert_eq!(tick_for_watchdog(tick, None), (tick, false));
        assert_eq!(
            tick_for_watchdog(tick, Some(Duration::from_secs(50))),
            (tick, false)
        );
        assert_eq!(
            tick_for_watchdog(tick, Some(Duration::from_secs(20))),
            (Duration::from_secs(20) / 3, true)
        );
        assert_eq!(
            tick_for_watchdog(tick, Some(Duration::from_secs(2))),
            (Duration::from_secs(1), true)
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_transport_is_a_no_op() {
        assert!(!send_to(OsStr::new("/run/systemd/notify"), "READY=1").unwrap());
        assert!(notify_socket().is_none());
        assert_eq!(watchdog_tick(true, false), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn datagram_reaches_a_bound_path_socket() {
        use std::os::unix::net::UnixDatagram;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify.sock");
        let rx = UnixDatagram::bind(&path).unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(send_to(path.as_os_str(), "WATCHDOG=1").unwrap());
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"WATCHDOG=1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn datagram_reaches_an_abstract_namespace_socket() {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        let name = format!("qontinui-sd-notify-test-{}", std::process::id());
        let rx = UnixDatagram::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes()).unwrap())
            .unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(send_to(OsStr::new(&format!("@{name}")), "READY=1").unwrap());
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1");
    }
}
