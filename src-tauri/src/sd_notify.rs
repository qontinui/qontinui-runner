//! systemd `sd_notify` integration for a runner run as a `Type=notify`
//! service with `WatchdogSec=` (plan
//! `2026-10-19-ci-nodes-run-the-tauri-runner-always-on-as-ci-node-role`,
//! Phase 2b).
//!
//! # What it sends, and from where
//!
//! - **`READY=1`**, once, when the HTTP API is bound and serving AND the
//!   ci_node `device_ci` coord subscription has connected. See
//!   [`ready_decision`] for the two cases that send it without the
//!   subscription (the ci_node runtime is disabled on this box, or the
//!   subscription has not connected within [`CI_SUBSCRIPTION_READY_GRACE`]).
//! - **`WATCHDOG=1`**, from the EXISTING runtime-independent `wedge-watchdog`
//!   OS thread's tick (`health_monitor::watchdog_loop`), never from the
//!   tokio-hosted self-probe loop. The decision is [`watchdog_decision`]:
//!   keep alive while the backend answers `/livez` and the probe thread's
//!   heartbeat is fresh; while it does NOT, keep alive only when restarting
//!   would destroy hosted work — the readiness gate below.
//!
//! # The readiness gate (served policy `production-and-cost` `runner-lifecycle`)
//!
//! A watchdog kill of a wedged runner is a stop of a running runner, which
//! the policy forbids while the runner hosts work. So a withheld keepalive
//! (which lets systemd kill and restart the process) happens only when the
//! last-good readiness snapshot says restart is SAFE. The snapshot is a
//! `/restart-readiness` verdict computed by the SAME sync
//! [`build_verdict`](crate::mcp::restart_readiness::build_verdict), published
//! by the async side into a std cell this module owns:
//!
//! - every background census pass (`tracking_health::run_once`) publishes a
//!   verdict built from its report ([`publish_census_report`]);
//! - every `GET /restart-readiness` publishes the verdict it served
//!   ([`publish_readiness_verdict`]).
//!
//! The newest of the two wins. The watchdog thread never calls the HTTP API
//! and never runs a census: both depend on the tokio runtime a wedge starves.
//! It only `try_lock`s the cell — a contended cell reads UNKNOWN and keeps
//! alive, so the watchdog thread can never block on it.
//!
//! Semantics: any session counted in the snapshot (an unsafe verdict, which
//! includes every UNKNOWN the verdict itself fails closed on) keeps the
//! runner alive; an empty (safe) snapshot withholds; a census that has never
//! completed is UNKNOWN and keeps alive (fail closed).
//!
//! **Known limit, stated rather than hidden:** the background census runs
//! every `tracking_health::CHECK_INTERVAL` (600 s), so a session opened after
//! the last pass and before a wedge is not in the snapshot unless a
//! `/restart-readiness` read published a fresher verdict. A ci_node runner
//! hosts no agent sessions by design, which is where this watchdog is armed.
//!
//! # Inert everywhere else
//!
//! Every send is a no-op when `NOTIFY_SOCKET` is unset — desktop runners,
//! dev runners, temp runners, tests. On a non-Linux build the transport
//! compiles to a no-op returning `Ok(false)`. The decision logic is
//! platform-independent so it is unit-tested on every OS.
//!
//! The transport is a direct datagram write (no crate): `NOTIFY_SOCKET` is a
//! filesystem path, or an abstract-namespace name spelled with a leading `@`.

use std::ffi::OsStr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use crate::mcp::restart_readiness::{self, RestartReadiness};
use crate::mcp::session_work_status::{self, SessionStatusSource};
use crate::session::tracking_health::TrackingHealthReport;

/// The environment variable systemd sets for a `Type=notify` service.
pub const NOTIFY_SOCKET_ENV: &str = "NOTIFY_SOCKET";

/// How long after the HTTP API is serving `READY=1` waits for the `device_ci`
/// subscription before it is sent anyway.
///
/// Why not wait forever: the subscription depends on coord being reachable.
/// A coord outage at boot would otherwise hold the unit in `activating` until
/// `TimeoutStartSec=` kills it, and `Restart=always` would turn one outage into
/// a start-limit crash loop of a runner whose local API was fine. The
/// subscription reconnects on its own back-off after READY.
pub const CI_SUBSCRIPTION_READY_GRACE: Duration = Duration::from_secs(90);

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// The `NOTIFY_SOCKET` value, read once. `None` (unset or empty) makes every
/// send a no-op.
fn notify_socket() -> Option<&'static std::ffi::OsString> {
    static SOCKET: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
    SOCKET
        .get_or_init(|| std::env::var_os(NOTIFY_SOCKET_ENV).filter(|s| !s.is_empty()))
        .as_ref()
}

/// Send `state` to the service manager. `Ok(false)` when there is no
/// `NOTIFY_SOCKET` (or on a non-Linux build); `Ok(true)` when the datagram
/// was written.
pub fn notify(state: &str) -> std::io::Result<bool> {
    match notify_socket() {
        Some(socket) => send_to(socket, state),
        None => Ok(false),
    }
}

/// Write one datagram to `socket` — a path, or `@name` for the abstract
/// namespace. The seam the Linux transport test drives.
#[cfg(target_os = "linux")]
pub fn send_to(socket: &OsStr, state: &str) -> std::io::Result<bool> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};

    let bytes = socket.as_bytes();
    let sock = UnixDatagram::unbound()?;
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

/// Pure READY decision. `serving_for` is how long the HTTP API has been
/// serving (`None` = not serving yet).
pub fn ready_decision(serving_for: Option<Duration>, ci: CiSubscription) -> Option<ReadyReason> {
    let serving_for = serving_for?;
    match ci {
        CiSubscription::Up => Some(ReadyReason::AllUp),
        CiSubscription::NotApplicable => Some(ReadyReason::CiNotApplicable),
        CiSubscription::Pending if serving_for >= CI_SUBSCRIPTION_READY_GRACE => {
            Some(ReadyReason::CiGraceElapsed)
        }
        CiSubscription::Pending => None,
    }
}

static HTTP_SERVING_SINCE: OnceLock<Instant> = OnceLock::new();
static CI_SUBSCRIPTION: AtomicU8 = AtomicU8::new(0);
static READY_SENT: AtomicBool = AtomicBool::new(false);

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

/// Send `READY=1` if it is due and not yet sent. Called from the mark sites
/// and from every watchdog tick (which is what fires the grace arm).
pub fn poll_ready() {
    if notify_socket().is_none() || READY_SENT.load(Ordering::SeqCst) {
        return;
    }
    let serving_for = HTTP_SERVING_SINCE.get().map(|t| t.elapsed());
    let ci = CiSubscription::from_u8(CI_SUBSCRIPTION.load(Ordering::SeqCst));
    let Some(reason) = ready_decision(serving_for, ci) else {
        return;
    };
    if READY_SENT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    match notify(&format!("READY=1\nSTATUS={}", reason.status())) {
        Ok(_) => tracing::info!("sd_notify: READY=1 sent ({})", reason.status()),
        Err(e) => {
            // Let a later poll retry rather than leaving the unit activating.
            READY_SENT.store(false, Ordering::SeqCst);
            tracing::warn!("sd_notify: READY=1 send failed ({e}); will retry");
        }
    }
}

// ---------------------------------------------------------------------------
// The last-good readiness snapshot
// ---------------------------------------------------------------------------

/// One published readiness verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedReadiness {
    /// Unix millis the evidence was taken.
    pub at_ms: i64,
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
    Published(PublishedReadiness),
}

static LAST_READINESS: Mutex<Option<PublishedReadiness>> = Mutex::new(None);

/// Store `p` into `cell` unless the cell already holds NEWER evidence.
fn publish_into(cell: &Mutex<Option<PublishedReadiness>>, p: PublishedReadiness) {
    let mut g = cell.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_none_or(|cur| p.at_ms >= cur.at_ms) {
        *g = Some(p);
    }
}

/// Read `cell` without ever blocking.
fn read_from(cell: &Mutex<Option<PublishedReadiness>>) -> ReadinessView {
    let g = match cell.try_lock() {
        Ok(g) => g,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        Err(TryLockError::WouldBlock) => return ReadinessView::Unreadable,
    };
    match g.as_ref() {
        Some(p) => ReadinessView::Published(p.clone()),
        None => ReadinessView::NeverRan,
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
            at_ms: report.checked_at_ms,
            safe_to_restart: v.safe_to_restart,
            reason: v.reason,
            source: "census",
        },
    );
}

/// Publish the verdict a `GET /restart-readiness` just served.
pub fn publish_readiness_verdict(v: &RestartReadiness, at_ms: i64) {
    publish_into(
        &LAST_READINESS,
        PublishedReadiness {
            at_ms,
            safe_to_restart: v.safe_to_restart,
            reason: v.reason.clone(),
            source: "restart-readiness",
        },
    );
}

/// The snapshot as the watchdog thread reads it. Never blocks.
pub fn readiness_view() -> ReadinessView {
    read_from(&LAST_READINESS)
}

// ---------------------------------------------------------------------------
// WATCHDOG=1
// ---------------------------------------------------------------------------

/// Why a keepalive is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepaliveReason {
    /// The backend answers `/livez` and the probe heartbeat is fresh.
    Healthy,
    /// Wedged, but the last-good snapshot counts live work.
    HostingWork,
    /// Wedged, and the snapshot is UNKNOWN (never ran, or unreadable).
    ReadinessUnknown,
}

/// One watchdog decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogDecision {
    Keepalive(KeepaliveReason),
    /// Wedged and the snapshot says restart is safe: send nothing, so
    /// systemd's `WatchdogSec=` expires and it restarts the process.
    Withhold,
}

/// Pure keepalive decision.
///
/// `backend_wedged` is `health_monitor::backend_wedged()`; `monitor_fresh` is
/// whether the self-probe thread's heartbeat is within
/// `WATCHDOG_MONITOR_STALL_SECS` (a stalled probe thread's `backend_wedged`
/// is stale, so it counts as not healthy).
pub fn watchdog_decision(
    backend_wedged: bool,
    monitor_fresh: bool,
    readiness: &ReadinessView,
) -> WatchdogDecision {
    if !backend_wedged && monitor_fresh {
        return WatchdogDecision::Keepalive(KeepaliveReason::Healthy);
    }
    match readiness {
        ReadinessView::NeverRan | ReadinessView::Unreadable => {
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        }
        ReadinessView::Published(p) if !p.safe_to_restart => {
            WatchdogDecision::Keepalive(KeepaliveReason::HostingWork)
        }
        ReadinessView::Published(_) => WatchdogDecision::Withhold,
    }
}

/// The datagram for a decision, or `None` when nothing is sent.
pub fn watchdog_message(decision: WatchdogDecision, readiness: &ReadinessView) -> Option<String> {
    let reason = match readiness {
        ReadinessView::Published(p) => p.reason.as_str(),
        ReadinessView::NeverRan => "no readiness census has completed yet",
        ReadinessView::Unreadable => "the readiness snapshot was contended this tick",
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
    let decision = watchdog_decision(backend_wedged, monitor_fresh, &readiness);
    if let Some(msg) = watchdog_message(decision, &readiness) {
        // Best-effort: a failed send is the same as a missed tick, which the
        // service manager already tolerates up to `WatchdogSec=`.
        let _ = notify(&msg);
    }
    Some(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(safe: bool, at_ms: i64) -> ReadinessView {
        ReadinessView::Published(PublishedReadiness {
            at_ms,
            safe_to_restart: safe,
            reason: if safe {
                "no live agent sessions in any plane".into()
            } else {
                "1 live session".into()
            },
            source: "census",
        })
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
            published(true, 1),
            published(false, 1),
        ] {
            assert_eq!(
                watchdog_decision(false, true, &view),
                WatchdogDecision::Keepalive(KeepaliveReason::Healthy)
            );
        }
    }

    #[test]
    fn wedged_and_safe_withholds_the_keepalive() {
        let view = published(true, 1);
        assert_eq!(
            watchdog_decision(true, true, &view),
            WatchdogDecision::Withhold
        );
        assert_eq!(watchdog_message(WatchdogDecision::Withhold, &view), None);
    }

    #[test]
    fn wedged_and_unsafe_keeps_alive() {
        let view = published(false, 1);
        let d = watchdog_decision(true, true, &view);
        assert_eq!(d, WatchdogDecision::Keepalive(KeepaliveReason::HostingWork));
        let msg = watchdog_message(d, &view).unwrap();
        assert!(msg.starts_with("WATCHDOG=1\n"), "{msg}");
    }

    #[test]
    fn a_stalled_probe_thread_is_not_healthy() {
        assert_eq!(
            watchdog_decision(false, false, &published(true, 1)),
            WatchdogDecision::Withhold
        );
        assert_eq!(
            watchdog_decision(false, false, &published(false, 1)),
            WatchdogDecision::Keepalive(KeepaliveReason::HostingWork)
        );
    }

    #[test]
    fn never_ran_census_is_unknown_and_keeps_alive() {
        let cell = Mutex::new(None);
        let view = read_from(&cell);
        assert_eq!(view, ReadinessView::NeverRan);
        assert_eq!(
            watchdog_decision(true, true, &view),
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        );
    }

    #[test]
    fn contended_snapshot_is_unknown_and_keeps_alive() {
        let cell = Mutex::new(None);
        let _held = cell.lock().unwrap();
        let view = read_from(&cell);
        assert_eq!(view, ReadinessView::Unreadable);
        assert_eq!(
            watchdog_decision(true, true, &view),
            WatchdogDecision::Keepalive(KeepaliveReason::ReadinessUnknown)
        );
    }

    #[test]
    fn snapshot_empty_withholds() {
        let now = 1_800_000_000_000;
        let v = census_verdict(&empty_report(now), now);
        assert!(v.safe_to_restart, "{}", v.reason);
        let cell = Mutex::new(None);
        publish_into(
            &cell,
            PublishedReadiness {
                at_ms: now,
                safe_to_restart: v.safe_to_restart,
                reason: v.reason,
                source: "census",
            },
        );
        assert_eq!(
            watchdog_decision(true, true, &read_from(&cell)),
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
        publish_into(
            &cell,
            PublishedReadiness {
                at_ms: now,
                safe_to_restart: v.safe_to_restart,
                reason: v.reason,
                source: "census",
            },
        );
        assert_eq!(
            watchdog_decision(true, true, &read_from(&cell)),
            WatchdogDecision::Keepalive(KeepaliveReason::HostingWork)
        );
    }

    #[test]
    fn older_evidence_never_replaces_newer() {
        let cell = Mutex::new(None);
        let p = |at_ms, safe| PublishedReadiness {
            at_ms,
            safe_to_restart: safe,
            reason: String::new(),
            source: "census",
        };
        publish_into(&cell, p(200, false));
        publish_into(&cell, p(100, true));
        assert_eq!(read_from(&cell), ReadinessView::Published(p(200, false)));
        publish_into(&cell, p(300, true));
        assert_eq!(read_from(&cell), ReadinessView::Published(p(300, true)));
    }

    #[test]
    fn ready_waits_for_http_and_the_subscription_within_grace() {
        let short = Some(Duration::from_secs(1));
        assert_eq!(ready_decision(None, CiSubscription::Up), None);
        assert_eq!(ready_decision(short, CiSubscription::Pending), None);
        assert_eq!(
            ready_decision(short, CiSubscription::Up),
            Some(ReadyReason::AllUp)
        );
        assert_eq!(
            ready_decision(short, CiSubscription::NotApplicable),
            Some(ReadyReason::CiNotApplicable)
        );
        assert_eq!(
            ready_decision(Some(CI_SUBSCRIPTION_READY_GRACE), CiSubscription::Pending),
            Some(ReadyReason::CiGraceElapsed)
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_transport_is_a_no_op() {
        assert!(!send_to(OsStr::new("/run/systemd/notify"), "READY=1").unwrap());
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
