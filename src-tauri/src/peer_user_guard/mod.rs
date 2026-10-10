//! Per-local-user connection guard for every runner TCP listener.
//!
//! Plan `2026-10-04-runner-loopback-api-refuses-other-local-users`, the
//! decision recorded in coord finding `c8a3d6b2-dcef-43b3-aa9e-7ff0660a59dd`.
//!
//! ## What it answers
//!
//! The runner's listeners bind the loopback, which keeps other MACHINES out
//! and nothing else: every local account could connect to `127.0.0.1:9876`
//! and reach the UI Bridge (which mints coord device JWTs), the coord-mcp
//! proxy, the file and hook doors and the terminals. On a CI host the GitHub
//! runner jobs run as a separate, non-admin account precisely so a job cannot
//! touch the owner's profile, and the loopback API was the one door that
//! account separation did not close.
//!
//! So on every accepted connection this module resolves the OS user that owns
//! the PEER socket and refuses the connection when that user differs from the
//! runner process's own:
//!
//! - **Linux** — the peer socket's uid by an exact `NETLINK_SOCK_DIAG` lookup of
//!   the full 4-tuple, accepted only for a CONNECTED socket whose echoed 4-tuple
//!   matches; a `/proc/net/tcp{,6}` scan is the fallback when netlink is
//!   unavailable ([`linux`]). A socket's uid is fixed at creation, so there is
//!   no PID to recycle.
//! - **Windows** — `GetExtendedTcpTable` → the live row matching the full 4-tuple →
//!   owning PID → process token → user SID ([`windows`]). The resolved
//!   process's creation time must precede the accept instant, so a recycled
//!   PID is refused rather than trusted.
//!
//! An owner that cannot be resolved is REFUSED (fail closed).
//!
//! ## Where it sits, and how it composes with the origin guard
//!
//! The check is a property of the CONNECTION, not of a request, so it lives in
//! the listener ([`GuardedListener`], an [`axum::serve::Listener`]) and a
//! refused peer gets no HTTP surface at all — no `/health`, no WebSocket
//! upgrade, no 404 body. The browser-origin guard
//! (`mcp::origin_guard`, binary crate) is HTTP middleware deciding what a
//! browser ORIGIN is evidence of; it only ever sees connections this guard
//! admitted. The two answer disjoint questions and share no code.
//!
//! ## Modes and kill switch
//!
//! [`ENV_KILL_SWITCH`] selects the [`Mode`]: `0` / `off` disables the guard,
//! `shadow` resolves every peer and COUNTS what it would refuse
//! (`shadowWouldRefuse` on `/health`) while admitting it, `1` / `enforce`
//! refuses. Unset — or an unrecognised value, with a WARN — takes the platform
//! default: `enforce` on Linux, `shadow` on Windows, where the runner's own
//! WebView2 network-service process is a caller whose token readability is
//! not yet measured (plan Phase 2). Read once, when a listener is wrapped.
//!
//! A platform with no resolver (macOS today) does not install the
//! guard and says so on `/health` (`supported: false`) — "cannot resolve this
//! one peer" fails closed, but "no resolver exists on this OS" would turn the
//! API off entirely, which is an outage, not a control.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;
// The pure row decoders are platform-independent so they are unit-tested on
// every CI leg, Windows logic included.
pub mod decode;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

/// Env var selecting the guard's [`Mode`]; `0` turns it off.
pub const ENV_KILL_SWITCH: &str = "QONTINUI_RUNNER_PEER_USER_GUARD";

/// What the guard does with a peer it would refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No resolution at all: today's behaviour.
    Off,
    /// Resolve and count would-refusals, but admit every peer.
    Shadow,
    /// Refuse peers that are not the runner's own user.
    Enforce,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Shadow => "shadow",
            Mode::Enforce => "enforce",
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Mode::Shadow,
            2 => Mode::Enforce,
            _ => Mode::Off,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Mode::Off => 0,
            Mode::Shadow => 1,
            Mode::Enforce => 2,
        }
    }
}

/// The mode used when the env var is unset or unrecognised.
pub fn platform_default_mode() -> Mode {
    if cfg!(windows) {
        Mode::Shadow
    } else {
        Mode::Enforce
    }
}

/// Parse the env value. Returns the mode and whether the value was
/// recognised (`false` means the platform default was taken for a value
/// someone set — worth a WARN).
pub fn mode_from_env_value(value: Option<&str>) -> (Mode, bool) {
    let Some(raw) = value else {
        return (platform_default_mode(), true);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => (platform_default_mode(), true),
        "0" | "off" => (Mode::Off, true),
        "shadow" => (Mode::Shadow, true),
        "1" | "enforce" | "on" => (Mode::Enforce, true),
        _ => (platform_default_mode(), false),
    }
}

/// Concurrent owner lookups per listener. A lookup is a `/proc` read or two
/// Win32 calls; this bounds the blocking pool a connection storm can occupy.
const MAX_INFLIGHT_RESOLUTIONS: usize = 64;

/// Distinct (reason, owner) pairs logged at WARN before the log goes quiet.
/// The counters keep counting past it.
const WARN_BUDGET: usize = 512;

/// An OS account, in the form the platform resolves it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LocalUser {
    /// A Unix uid (the socket's owner on Linux).
    Uid(u32),
    /// A Windows SID in its canonical string form (`S-1-5-21-…`). Two SIDs are
    /// equal exactly when their canonical strings are, so this compares like
    /// `EqualSid`.
    Sid(String),
}

impl std::fmt::Display for LocalUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalUser::Uid(u) => write!(f, "uid:{u}"),
            LocalUser::Sid(s) => write!(f, "sid:{s}"),
        }
    }
}

/// Why a peer's owner could not be established. Every variant is a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unresolved {
    /// No table row carries this connection's 4-tuple.
    NoMatchingSocket,
    /// The row exists but is closing (`TIME_WAIT` / orphaned, inode 0): its
    /// recorded owner is not the process that connected.
    SocketClosing,
    /// The owning process could not be opened (Windows).
    ProcessUnopenable,
    /// The owning process's token or user could not be read (Windows).
    TokenUnreadable,
    /// `OpenProcessToken` was refused with ACCESS_DENIED — expected for an
    /// ELEVATED caller of the same account seen from a non-elevated runner,
    /// and for a protected process (Windows). Counted apart so shadow mode
    /// measures it before Windows enforces.
    TokenAccessDenied,
    /// The owning PID belongs to a process created after the connection was
    /// accepted — a recycled PID (Windows).
    PidRecycled,
    /// The socket table itself could not be read.
    TableUnreadable,
    /// The runner's OWN user could not be established, so no peer can be
    /// compared against it.
    OwnUserUnknown,
}

impl Unresolved {
    pub fn as_str(self) -> &'static str {
        match self {
            Unresolved::NoMatchingSocket => "no_matching_socket",
            Unresolved::SocketClosing => "socket_closing",
            Unresolved::ProcessUnopenable => "process_unopenable",
            Unresolved::TokenUnreadable => "token_unreadable",
            Unresolved::TokenAccessDenied => "token_access_denied",
            Unresolved::PidRecycled => "pid_recycled",
            Unresolved::TableUnreadable => "table_unreadable",
            Unresolved::OwnUserUnknown => "own_user_unknown",
        }
    }
}

/// What a resolver found for one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub owner: Result<LocalUser, Unresolved>,
    /// The owning PID where the platform reports one (Windows); for the log.
    pub pid: Option<u32>,
}

/// Resolves OS users. Injected so the decision and the listener are tested
/// without a second account.
pub trait OwnerResolver: Send + Sync + 'static {
    /// The user the runner process itself runs as.
    fn own_user(&self) -> Result<LocalUser, String>;
    /// The user owning the peer end of the connection `peer → local`,
    /// accepted at `accepted_at`.
    fn resolve(&self, local: SocketAddr, peer: SocketAddr, accepted_at: SystemTime) -> Resolution;
}

/// The guard's verdict on one connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Admit,
    RefuseOtherUser(LocalUser),
    RefuseUnresolved(Unresolved),
}

/// The decision: admit only a positively resolved owner equal to `own`.
pub fn decide(own: Option<&LocalUser>, resolution: &Resolution) -> Verdict {
    let Some(own) = own else {
        return Verdict::RefuseUnresolved(Unresolved::OwnUserUnknown);
    };
    match &resolution.owner {
        Ok(peer) if peer == own => Verdict::Admit,
        Ok(peer) => Verdict::RefuseOtherUser(peer.clone()),
        Err(why) => Verdict::RefuseUnresolved(*why),
    }
}

/// The OS resolver for this platform, or `None` where none exists.
pub fn os_resolver() -> Option<Arc<dyn OwnerResolver>> {
    #[cfg(target_os = "linux")]
    {
        Some(Arc::new(linux::ProcNetResolver))
    }
    #[cfg(windows)]
    {
        Some(Arc::new(windows::TcpTableResolver))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Counters — process-wide, shared by every wrapped listener.
// ---------------------------------------------------------------------------

struct Stats {
    installed: AtomicBool,
    supported: AtomicBool,
    /// The strictest mode any wrapped listener runs in ([`Mode::as_u8`]).
    mode: AtomicU8,
    admitted: AtomicU64,
    refused_other_user: AtomicU64,
    refused_unresolved: AtomicU64,
    shadow_other_user: AtomicU64,
    shadow_unresolved: AtomicU64,
}

static STATS: Stats = Stats {
    installed: AtomicBool::new(false),
    supported: AtomicBool::new(true),
    mode: AtomicU8::new(0),
    admitted: AtomicU64::new(0),
    refused_other_user: AtomicU64::new(0),
    refused_unresolved: AtomicU64::new(0),
    shadow_other_user: AtomicU64::new(0),
    shadow_unresolved: AtomicU64::new(0),
};

fn warn_seen() -> &'static Mutex<HashSet<String>> {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// `/health`'s `peerUserGuard` block. Counters only: no uid or SID is served,
/// because `/health` is reachable from browser origins. Identities go to the
/// log. The counters are process-local, so a zero right after a start is
/// UNKNOWN, never evidence that nothing was refused before it.
pub fn health_json() -> serde_json::Value {
    let mode = Mode::from_u8(STATS.mode.load(Ordering::Relaxed));
    serde_json::json!({
        "installed": STATS.installed.load(Ordering::Relaxed),
        "enabled": mode != Mode::Off,
        "mode": mode.as_str(),
        "supported": STATS.supported.load(Ordering::Relaxed),
        "killSwitchEnv": ENV_KILL_SWITCH,
        "admitted": STATS.admitted.load(Ordering::Relaxed),
        "refusals": {
            "otherUser": STATS.refused_other_user.load(Ordering::Relaxed),
            "unresolved": STATS.refused_unresolved.load(Ordering::Relaxed),
        },
        "shadowWouldRefuse": {
            "otherUser": STATS.shadow_other_user.load(Ordering::Relaxed),
            "unresolved": STATS.shadow_unresolved.load(Ordering::Relaxed),
        },
    })
}

fn record(label: &str, peer: SocketAddr, verdict: &Verdict, pid: Option<u32>, mode: Mode) {
    let shadow = mode == Mode::Shadow;
    let (reason, owner) = match verdict {
        Verdict::Admit => {
            STATS.admitted.fetch_add(1, Ordering::Relaxed);
            return;
        }
        Verdict::RefuseOtherUser(u) => {
            let c = if shadow {
                &STATS.shadow_other_user
            } else {
                &STATS.refused_other_user
            };
            c.fetch_add(1, Ordering::Relaxed);
            ("other_user", u.to_string())
        }
        Verdict::RefuseUnresolved(why) => {
            let c = if shadow {
                &STATS.shadow_unresolved
            } else {
                &STATS.refused_unresolved
            };
            c.fetch_add(1, Ordering::Relaxed);
            (why.as_str(), "unresolved".to_string())
        }
    };
    if shadow {
        // A would-refuse is admitted; it still counts as an admission.
        STATS.admitted.fetch_add(1, Ordering::Relaxed);
    }
    let action = if shadow {
        "WOULD refuse (shadow mode, admitted)"
    } else {
        "refused"
    };
    debug!(listener = label, %peer, reason, owner = %owner, pid, action, "peer user guard");
    let key = format!("{label}|{action}|{reason}|{owner}");
    let mut seen = match warn_seen().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if seen.contains(&key) {
        return;
    }
    if seen.len() >= WARN_BUDGET {
        if seen.len() == WARN_BUDGET {
            seen.insert(String::from("<budget-spent>"));
            warn!(
                "peer user guard: WARN log budget spent ({WARN_BUDGET} distinct refusals); \
                 /health peerUserGuard.refusals keeps counting"
            );
        }
        return;
    }
    seen.insert(key);
    warn!(
        listener = label,
        %peer,
        reason,
        owner = %owner,
        pid,
        action,
        kill_switch = ENV_KILL_SWITCH,
        "peer user guard: a loopback connection from a process that is not provably \
         running as this runner's OS user (first sighting of this reason+owner)"
    );
}

// ---------------------------------------------------------------------------
// The listener
// ---------------------------------------------------------------------------

type Admitted = (TcpStream, SocketAddr);

enum Inner {
    /// Guard off (kill switch) or unsupported platform: today's behaviour.
    Passthrough(TcpListener),
    Guarded(Box<Guarded>),
}

/// The guarded arm. The listener is OWNED here, not by a background task, so
/// dropping the [`GuardedListener`] closes the socket synchronously (the
/// Cognito callback re-binds its fixed port right after shutdown), and the
/// in-flight resolutions live in a [`JoinSet`] that aborts them on drop.
struct Guarded {
    listener: TcpListener,
    label: &'static str,
    resolver: Arc<dyn OwnerResolver>,
    own: Arc<Option<LocalUser>>,
    mode: Mode,
    pending: JoinSet<Option<Admitted>>,
    /// Set after a non-connection accept error (EMFILE / ENFILE …): the accept
    /// side pauses until then while finished resolutions keep draining.
    backoff_until: Option<tokio::time::Instant>,
}

/// A [`TcpListener`] that only yields connections whose peer runs as the
/// runner's own OS user. Use it in place of the listener passed to
/// `axum::serve`. `accept` must be polled inside a tokio runtime (it spawns
/// the per-connection resolutions there), which `axum::serve` always does.
pub struct GuardedListener {
    inner: Inner,
    local: SocketAddr,
}

impl GuardedListener {
    /// Wrap `listener` with the OS resolver and the env-selected [`Mode`].
    pub fn wrap(listener: TcpListener, label: &'static str) -> std::io::Result<Self> {
        let raw = std::env::var(ENV_KILL_SWITCH).ok();
        let (mode, recognised) = mode_from_env_value(raw.as_deref());
        if !recognised {
            warn!(
                listener = label,
                value = raw.as_deref().unwrap_or(""),
                kill_switch = ENV_KILL_SWITCH,
                default = platform_default_mode().as_str(),
                "peer user guard: unrecognised mode value — using the platform default \
                 (accepted: 0/off, shadow, 1/enforce)"
            );
        }
        match os_resolver() {
            Some(resolver) => Self::with_resolver(listener, label, resolver, mode),
            None => {
                STATS.supported.store(false, Ordering::Relaxed);
                warn!(
                    listener = label,
                    "peer user guard: no owner resolver on this OS — the guard is NOT \
                     installed and every local user can reach this listener"
                );
                Self::with_resolver_opt(listener, label, None, Mode::Off)
            }
        }
    }

    /// Wrap with an explicit resolver and mode (tests inject both).
    pub fn with_resolver(
        listener: TcpListener,
        label: &'static str,
        resolver: Arc<dyn OwnerResolver>,
        mode: Mode,
    ) -> std::io::Result<Self> {
        Self::with_resolver_opt(listener, label, Some(resolver), mode)
    }

    fn with_resolver_opt(
        listener: TcpListener,
        label: &'static str,
        resolver: Option<Arc<dyn OwnerResolver>>,
        mode: Mode,
    ) -> std::io::Result<Self> {
        let local = listener.local_addr()?;
        let resolver = match (resolver, mode) {
            (Some(r), Mode::Shadow | Mode::Enforce) => r,
            (Some(_), Mode::Off) => {
                warn!(
                    listener = label,
                    kill_switch = ENV_KILL_SWITCH,
                    "peer user guard: DISABLED by kill switch — every local user can \
                     reach this listener"
                );
                return Ok(Self {
                    inner: Inner::Passthrough(listener),
                    local,
                });
            }
            (None, _) => {
                return Ok(Self {
                    inner: Inner::Passthrough(listener),
                    local,
                })
            }
        };
        let own = match resolver.own_user() {
            Ok(u) => {
                info!(listener = label, %local, own_user = %u, mode = mode.as_str(), "peer user guard: installed");
                Some(u)
            }
            Err(e) => {
                error!(
                    listener = label,
                    error = %e,
                    mode = mode.as_str(),
                    "peer user guard: could not resolve this runner's own OS user — \
                     every connection is a would-refuse (refused under enforce)"
                );
                None
            }
        };
        STATS.installed.store(true, Ordering::Relaxed);
        STATS.mode.fetch_max(mode.as_u8(), Ordering::Relaxed);
        Ok(Self {
            inner: Inner::Guarded(Box::new(Guarded {
                listener,
                label,
                resolver,
                own: Arc::new(own),
                mode,
                pending: JoinSet::new(),
                backoff_until: None,
            })),
            local,
        })
    }
}

/// Resolve one accepted connection and return it when it is admitted.
async fn resolve_one(
    stream: TcpStream,
    peer: SocketAddr,
    accepted_at: SystemTime,
    label: &'static str,
    resolver: Arc<dyn OwnerResolver>,
    own: Arc<Option<LocalUser>>,
    mode: Mode,
) -> Option<Admitted> {
    let resolution = match (own.as_ref(), stream.local_addr()) {
        // No own user: skip the lookup, the decision refuses anyway.
        (None, _) => Resolution {
            owner: Err(Unresolved::OwnUserUnknown),
            pid: None,
        },
        (Some(_), Err(_)) => Resolution {
            owner: Err(Unresolved::NoMatchingSocket),
            pid: None,
        },
        (Some(_), Ok(local)) => crate::wedge_diagnostics::spawn_blocking_tracked(move || {
            resolver.resolve(local, peer, accepted_at)
        })
        .await
        .unwrap_or(Resolution {
            owner: Err(Unresolved::TableUnreadable),
            pid: None,
        }),
    };
    let verdict = decide(own.as_ref().as_ref(), &resolution);
    record(label, peer, &verdict, resolution.pid, mode);
    // A refused stream is dropped here: the peer sees the connection close
    // before a single byte.
    (verdict == Verdict::Admit || mode == Mode::Shadow).then_some((stream, peer))
}

impl Guarded {
    async fn accept(&mut self) -> Admitted {
        loop {
            tokio::select! {
                // Collect a finished resolution first, so admitted peers are
                // served before more work is taken on.
                biased;
                Some(done) = self.pending.join_next() => {
                    // A resolution task that panicked is a refusal.
                    if let Ok(Some(admitted)) = done {
                        return admitted;
                    }
                }
                () = tokio::time::sleep_until(self.backoff_until.unwrap_or_else(tokio::time::Instant::now)), if self.backoff_until.is_some() => {
                    self.backoff_until = None;
                }
                accepted = self.listener.accept(), if self.backoff_until.is_none() && self.pending.len() < MAX_INFLIGHT_RESOLUTIONS => {
                    match accepted {
                        Ok((stream, peer)) => {
                            // Stamp the accept instant now, so the PID-recycle
                            // check compares against when the connection existed.
                            let accepted_at = SystemTime::now();
                            self.pending.spawn(resolve_one(
                                stream,
                                peer,
                                accepted_at,
                                self.label,
                                self.resolver.clone(),
                                self.own.clone(),
                                self.mode,
                            ));
                        }
                        // Same policy as axum's own `TcpListener` accept: a
                        // per-connection error is skipped, anything else backs off.
                        Err(e) if is_connection_error(&e) => {}
                        Err(e) => {
                            warn!(listener = self.label, error = %e, "peer user guard: accept error");
                            self.backoff_until = Some(tokio::time::Instant::now() + Duration::from_secs(1));
                        }
                    }
                }
            }
        }
    }
}

fn is_connection_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
    )
}

impl axum::serve::Listener for GuardedListener {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match &mut self.inner {
            Inner::Passthrough(l) => axum::serve::Listener::accept(l).await,
            Inner::Guarded(g) => g.accept().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}
