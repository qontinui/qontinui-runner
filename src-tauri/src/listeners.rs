//! Inherited listening sockets: the `sd_listen_fds(3)` protocol, the one
//! `adopt_or_bind` every fixed listener goes through, and the HTTP liveness
//! probe that replaces "a TCP connect succeeded" once a socket can outlive the
//! process that serves it.
//!
//! Plan `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! D1 and Phase 2.
//!
//! ## Why
//!
//! A runner upgrade used to leave `127.0.0.1:9876` unbound for the 15-45 s the
//! successor took to boot, so every hook, coord-mcp proxy call and forwarder
//! write made then was refused. If the listening socket is owned by something
//! that outlives the process — a systemd socket unit, or (later) the
//! predecessor handing its fds to its successor — the kernel queues connects in
//! the backlog instead, and a client sees a slow call, never `ECONNREFUSED`.
//! Both mechanisms speak ONE env protocol, `LISTEN_PID` / `LISTEN_FDS` /
//! `LISTEN_FDNAMES` (fds `3..3+LISTEN_FDS`), so the runner has a single
//! "adopt inherited listeners" path; [`parse_listen_env`] and
//! [`listen_env_for`] are both halves of it.
//!
//! ## The order of events in a process
//!
//! 1. `main()`, while still single-threaded, calls
//!    [`adopt_inherited_at_startup`]: it takes ownership of every fd addressed
//!    to this pid, marks each close-on-exec, and removes the three variables
//!    from the environment so no child believes the fds are its own. Nothing is
//!    validated or served yet, and nothing is logged (logging does not exist).
//! 2. Once logging exists, [`log_startup_adoption`] reports what was taken.
//! 3. The listener's owner calls [`adopt_or_bind`] (or [`take_inherited`] when
//!    it must not bind): the fd with that name is checked to be a listening TCP
//!    socket bound to exactly the expected address and handed over as a
//!    non-blocking `std::net::TcpListener`; otherwise the address is bound the
//!    way the runner always bound it ([`bind_reuseaddr`]).
//!
//! Adoption is unix-only. On every other OS [`adopt_inherited_at_startup`]
//! takes nothing and [`adopt_or_bind`] always binds (Windows listener passing
//! is a later phase of the plan).
//!
//! ## Why a TCP connect no longer means "alive"
//!
//! With a socket unit holding the port, a runner that is dead or crash-looping
//! still ACCEPTS connects into the backlog. Any probe that decides "is a runner
//! there" from a connect alone would call a dead runner alive. [`probe_livez`]
//! asks `GET /livez` instead, under a deadline, and distinguishes a booting
//! runner (`503 RUNNER_BOOTING`) from a live one and from nothing answering.
//! A connect that is accepted and then answers nothing is `Silent`, which a
//! probe cannot tell from a live runner too loaded to answer, so callers that
//! destroy state on "dead" keep it ([`LivezVerdict::may_be_alive`]).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The systemd `FileDescriptorName=` of the runner's HTTP API listener.
pub const API_LISTENER_NAME: &str = "api";

/// The pid the inherited fds are addressed to.
pub const ENV_LISTEN_PID: &str = "LISTEN_PID";
/// How many fds were passed, starting at [`SD_LISTEN_FDS_START`].
pub const ENV_LISTEN_FDS: &str = "LISTEN_FDS";
/// Colon-separated names, one per fd, in fd order.
pub const ENV_LISTEN_FDNAMES: &str = "LISTEN_FDNAMES";

/// The first inherited fd (`SD_LISTEN_FDS_START` in `sd-daemon.h`).
pub const SD_LISTEN_FDS_START: i32 = 3;

/// The name an fd carries when `LISTEN_FDNAMES` is absent (as in
/// `sd_listen_fds_with_names(3)`).
pub const UNKNOWN_FD_NAME: &str = "unknown";

/// The `ApiResponse` error code the runner answers with while its listener is
/// adopted but its router is not built yet.
pub const RUNNER_BOOTING_CODE: &str = "RUNNER_BOOTING";

/// The three protocol variables, in the order they are cleared.
const LISTEN_ENV_KEYS: [&str; 3] = [ENV_LISTEN_PID, ENV_LISTEN_FDS, ENV_LISTEN_FDNAMES];

/// The fds addressed to this process: fd `start + i` carries `names[i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenFds {
    pub start: i32,
    pub names: Vec<String>,
}

impl ListenFds {
    /// `(fd, name)` pairs in fd order.
    pub fn fds(&self) -> impl Iterator<Item = (i32, &str)> + '_ {
        self.names
            .iter()
            .enumerate()
            .map(move |(i, name)| (self.start + i as i32, name.as_str()))
    }
}

/// Why the protocol variables could not be read. Every arm means the fds are
/// NOT adopted; the variables are still cleared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenEnvError {
    /// `LISTEN_PID` is not a pid.
    BadPid(String),
    /// `LISTEN_FDS` is not a non-negative count, or would run past the fd
    /// range.
    BadCount(String),
    /// `LISTEN_FDNAMES` names a different number of fds than `LISTEN_FDS`
    /// passed (`sd_listen_fds_with_names(3)` refuses this too).
    NameCountMismatch { fds: usize, names: usize },
}

impl std::fmt::Display for ListenEnvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPid(v) => write!(f, "{ENV_LISTEN_PID}={v:?} is not a pid"),
            Self::BadCount(v) => write!(f, "{ENV_LISTEN_FDS}={v:?} is not a usable fd count"),
            Self::NameCountMismatch { fds, names } => write!(
                f,
                "{ENV_LISTEN_FDNAMES} names {names} fd(s) but {ENV_LISTEN_FDS} passed {fds}"
            ),
        }
    }
}

/// Parse the `sd_listen_fds(3)` variables. Pure: the values and our pid are
/// arguments, so a self-managed predecessor's encoding ([`listen_env_for`]) and
/// systemd's are read by the same code and tested without a fork.
///
/// `Ok(None)` when nothing is addressed to `our_pid`: no `LISTEN_PID`, a
/// `LISTEN_PID` naming another process (the variables leaked from an ancestor),
/// no `LISTEN_FDS`, or `LISTEN_FDS=0`.
pub fn parse_listen_env(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    listen_fdnames: Option<&str>,
    our_pid: u32,
) -> Result<Option<ListenFds>, ListenEnvError> {
    let Some(pid) = listen_pid else {
        return Ok(None);
    };
    let pid: u32 = pid
        .trim()
        .parse()
        .map_err(|_| ListenEnvError::BadPid(pid.to_string()))?;
    if pid != our_pid {
        return Ok(None);
    }
    let Some(count) = listen_fds else {
        return Ok(None);
    };
    let n: usize = count
        .trim()
        .parse()
        .map_err(|_| ListenEnvError::BadCount(count.to_string()))?;
    if n == 0 {
        return Ok(None);
    }
    if n > (i32::MAX - SD_LISTEN_FDS_START) as usize {
        return Err(ListenEnvError::BadCount(count.to_string()));
    }
    let names: Vec<String> = match listen_fdnames {
        None => vec![UNKNOWN_FD_NAME.to_string(); n],
        Some(raw) => {
            let names: Vec<String> = raw.split(':').map(str::to_string).collect();
            if names.len() != n {
                return Err(ListenEnvError::NameCountMismatch {
                    fds: n,
                    names: names.len(),
                });
            }
            names
        }
    };
    Ok(Some(ListenFds {
        start: SD_LISTEN_FDS_START,
        names,
    }))
}

/// The variables a process sets for a child it hands `names.len()` listeners
/// to, as fds `3..`, in `names` order. `pid` is the CHILD's pid, so a spawner
/// sets these between fork and exec (where the child's pid is known); this is
/// the encoding [`parse_listen_env`] reads.
pub fn listen_env_for(pid: u32, names: &[&str]) -> [(&'static str, String); 3] {
    [
        (ENV_LISTEN_PID, pid.to_string()),
        (ENV_LISTEN_FDS, names.len().to_string()),
        (ENV_LISTEN_FDNAMES, names.join(":")),
    ]
}

/// Where the protocol variables are read from and removed. The process
/// environment in production ([`ProcessEnv`]); a map in tests.
pub trait ListenEnv {
    fn get(&self, key: &str) -> Option<String>;
    fn remove(&mut self, key: &str);
}

/// The real process environment. Removing a variable is only sound while the
/// process is single-threaded, which is why [`adopt_inherited_at_startup`]
/// runs from `main()` before any runtime exists.
pub struct ProcessEnv;

impl ListenEnv for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
    fn remove(&mut self, key: &str) {
        std::env::remove_var(key);
    }
}

impl ListenEnv for HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        HashMap::get(self, key).cloned()
    }
    fn remove(&mut self, key: &str) {
        HashMap::remove(self, key);
    }
}

/// How a listener came to be held by this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerOrigin {
    /// Taken from an inherited fd (socket activation or a predecessor's
    /// handoff). It has never been unbound across the process change.
    Inherited,
    /// Bound by this process.
    Bound,
}

impl ListenerOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherited => "inherited",
            Self::Bound => "bound",
        }
    }
}

/// What [`adopt_inherited_at_startup`] found, kept for the log line written
/// once logging exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAdoption {
    /// No variables addressed fds to this process.
    NothingInherited,
    /// The named fds were taken (an fd that was not open is listed in
    /// `not_open` and was not taken).
    Taken {
        names: Vec<String>,
        not_open: Vec<i32>,
    },
    /// The variables were present and unreadable; nothing was taken.
    Refused(ListenEnvError),
    /// This OS has no adoption path; nothing was taken.
    Unsupported,
}

/// Inherited fds this process owns and has not handed to a listener yet.
#[derive(Default)]
pub struct InheritedListeners {
    #[cfg(unix)]
    fds: Mutex<Vec<(String, std::os::fd::OwnedFd)>>,
}

impl InheritedListeners {
    /// An empty store (nothing inherited).
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the protocol variables from `env`, take ownership of every fd they
    /// address to `our_pid`, mark each close-on-exec, and remove the three
    /// variables from `env` — whether or not anything was taken, as
    /// `sd_listen_fds(3)` does with `unset_environment`. `fd_start` overrides
    /// `SD_LISTEN_FDS_START` so a test can hand over an fd it opened itself.
    #[cfg(unix)]
    pub fn from_env(
        env: &mut dyn ListenEnv,
        our_pid: u32,
        fd_start: i32,
    ) -> (Self, StartupAdoption) {
        use std::os::fd::{FromRawFd, OwnedFd};

        let parsed = parse_listen_env(
            env.get(ENV_LISTEN_PID).as_deref(),
            env.get(ENV_LISTEN_FDS).as_deref(),
            env.get(ENV_LISTEN_FDNAMES).as_deref(),
            our_pid,
        );
        for key in LISTEN_ENV_KEYS {
            env.remove(key);
        }
        let store = Self::new();
        let outcome = match parsed {
            Ok(None) => StartupAdoption::NothingInherited,
            Err(e) => StartupAdoption::Refused(e),
            Ok(Some(mut listen)) => {
                listen.start = fd_start;
                let mut names = Vec::new();
                let mut not_open = Vec::new();
                let mut fds = store.fds.lock().unwrap_or_else(|p| p.into_inner());
                for (fd, name) in listen.fds() {
                    if !set_cloexec(fd) {
                        not_open.push(fd);
                        continue;
                    }
                    // SAFETY: `fd` is open (F_GETFD/F_SETFD just succeeded on
                    // it) and was passed to THIS pid by the protocol, so this
                    // process is its owner; nothing else in the process holds it
                    // because this runs before anything opens a file.
                    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
                    names.push(name.to_string());
                    fds.push((name.to_string(), owned));
                }
                drop(fds);
                StartupAdoption::Taken { names, not_open }
            }
        };
        (store, outcome)
    }

    /// Names of the fds still held, in fd order.
    pub fn names(&self) -> Vec<String> {
        #[cfg(unix)]
        {
            self.fds
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .map(|(n, _)| n.clone())
                .collect()
        }
        #[cfg(not(unix))]
        {
            Vec::new()
        }
    }

    /// Whether an fd named `name` is still held.
    pub fn contains(&self, name: &str) -> bool {
        self.names().iter().any(|n| n == name)
    }

    /// Remove the first fd named `name` and hand it over as a non-blocking
    /// `TcpListener`, after checking it IS a listening TCP socket bound to
    /// exactly `expected`. `Ok(None)` when no fd has that name. On `Err` the fd
    /// has been closed: it was ours, it is unusable, and keeping it open would
    /// only hold a port nobody serves.
    pub fn take(&self, name: &str, expected: SocketAddr) -> std::io::Result<Option<TcpListener>> {
        #[cfg(unix)]
        {
            let fd = {
                let mut fds = self.fds.lock().unwrap_or_else(|p| p.into_inner());
                match fds.iter().position(|(n, _)| n == name) {
                    Some(i) => fds.remove(i).1,
                    None => return Ok(None),
                }
            };
            listener_from_inherited(fd, expected).map(Some)
        }
        #[cfg(not(unix))]
        {
            let _ = (name, expected);
            Ok(None)
        }
    }
}

/// Set `FD_CLOEXEC` on `fd`. `false` when `fd` is not open.
#[cfg(unix)]
fn set_cloexec(fd: i32) -> bool {
    // SAFETY: fcntl on an arbitrary integer is defined: a closed fd answers
    // EBADF, which is the "not open" arm.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return false;
    }
    // SAFETY: as above; `fd` was just shown to be open.
    unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) >= 0 }
}

/// Check an inherited fd and turn it into a non-blocking std listener.
#[cfg(unix)]
fn listener_from_inherited(
    fd: std::os::fd::OwnedFd,
    expected: SocketAddr,
) -> std::io::Result<TcpListener> {
    use std::os::fd::AsRawFd;

    let socket = socket2::Socket::from(fd);
    let invalid = |what: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, what);
    let kind = socket.r#type()?;
    if kind != socket2::Type::STREAM {
        return Err(invalid(format!(
            "inherited fd {} is not a stream socket ({kind:?})",
            socket.as_raw_fd()
        )));
    }
    if !is_listening(&socket)? {
        return Err(invalid(format!(
            "inherited fd {} is a stream socket that is not listening",
            socket.as_raw_fd()
        )));
    }
    let local = socket.local_addr()?.as_socket().ok_or_else(|| {
        invalid(format!(
            "inherited fd {} is not an IP socket",
            socket.as_raw_fd()
        ))
    })?;
    if local != expected {
        return Err(invalid(format!(
            "inherited fd {} is bound to {local}, not the expected {expected}",
            socket.as_raw_fd()
        )));
    }
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

/// `SO_ACCEPTCONN`: whether `listen(2)` was called on the socket.
#[cfg(unix)]
fn is_listening(socket: &socket2::Socket) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `value`/`len` are a correctly sized out-buffer for an int option.
    let rc = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ACCEPTCONN,
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(value != 0)
}

/// Bind `addr` the way the runner has always bound its API listener:
/// `SO_REUSEADDR` (so a `TIME_WAIT`/`CLOSE_WAIT` leftover on Windows does not
/// block the bind), non-blocking, backlog 1024.
pub fn bind_reuseaddr(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    Ok(socket.into())
}

/// A listener and how it was obtained.
#[derive(Debug)]
pub struct AcquiredListener {
    pub listener: TcpListener,
    pub origin: ListenerOrigin,
}

/// [`adopt_or_bind`] against an explicit store and bind function (tests).
///
/// An inherited fd that fails its checks is reported and closed, then `addr` is
/// bound — a socket unit pointed at the wrong address must not cost the runner
/// its API.
pub fn adopt_or_bind_from(
    store: &InheritedListeners,
    name: &str,
    addr: SocketAddr,
    bind: impl FnOnce(SocketAddr) -> std::io::Result<TcpListener>,
) -> std::io::Result<AcquiredListener> {
    match store.take(name, addr) {
        Ok(Some(listener)) => {
            return Ok(AcquiredListener {
                listener,
                origin: ListenerOrigin::Inherited,
            })
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(
            listener = name,
            %addr,
            error = %e,
            "inherited listener rejected and closed — binding the address instead"
        ),
    }
    Ok(AcquiredListener {
        listener: bind(addr)?,
        origin: ListenerOrigin::Bound,
    })
}

/// Take the inherited fd named `name` when one was passed to this process and
/// is a listening TCP socket on `addr`; otherwise bind `addr`
/// ([`bind_reuseaddr`]). The result is recorded for `/health`
/// ([`health_json`]).
pub fn adopt_or_bind(name: &str, addr: SocketAddr) -> std::io::Result<AcquiredListener> {
    let acquired = adopt_or_bind_from(inherited(), name, addr, bind_reuseaddr)?;
    record_origin(name, acquired.origin);
    Ok(acquired)
}

/// Take the inherited fd named `name` WITHOUT a bind fallback: for the early
/// boot path, which may only act on an adopted socket. Recorded for `/health`
/// when taken.
pub fn take_inherited(name: &str, addr: SocketAddr) -> std::io::Result<Option<TcpListener>> {
    let taken = inherited().take(name, addr)?;
    if taken.is_some() {
        record_origin(name, ListenerOrigin::Inherited);
    }
    Ok(taken)
}

/// Whether an fd named `name` was inherited and not taken yet.
pub fn has_inherited(name: &str) -> bool {
    inherited().contains(name)
}

static INHERITED: OnceLock<InheritedListeners> = OnceLock::new();
static STARTUP_ADOPTION: OnceLock<StartupAdoption> = OnceLock::new();
static ORIGINS: OnceLock<Mutex<HashMap<String, ListenerOrigin>>> = OnceLock::new();

fn inherited() -> &'static InheritedListeners {
    INHERITED.get_or_init(InheritedListeners::new)
}

fn origins() -> &'static Mutex<HashMap<String, ListenerOrigin>> {
    ORIGINS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record how the listener `name` was obtained (read by [`health_json`]).
pub fn record_origin(name: &str, origin: ListenerOrigin) {
    origins()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(name.to_string(), origin);
}

/// How the listener `name` was obtained, or `None` before it was.
pub fn origin_of(name: &str) -> Option<ListenerOrigin> {
    origins()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(name)
        .copied()
}

/// Take every fd the process environment addresses to this pid and clear the
/// protocol variables. Call ONCE, from `main()`, while the process is still
/// single-threaded (it removes environment variables). Idempotent: a second
/// call takes nothing.
pub fn adopt_inherited_at_startup() {
    // Guard on the adoption record, not on INHERITED: `inherited()` lazily
    // initialises that cell, so an accessor called before this would otherwise
    // turn adoption off silently.
    if STARTUP_ADOPTION.get().is_some() {
        return;
    }
    #[cfg(unix)]
    let (store, outcome) =
        InheritedListeners::from_env(&mut ProcessEnv, std::process::id(), SD_LISTEN_FDS_START);
    #[cfg(not(unix))]
    let (store, outcome) = {
        let mut env = ProcessEnv;
        let present = LISTEN_ENV_KEYS.iter().any(|k| env.get(k).is_some());
        for key in LISTEN_ENV_KEYS {
            env.remove(key);
        }
        (
            InheritedListeners::new(),
            if present {
                StartupAdoption::Unsupported
            } else {
                StartupAdoption::NothingInherited
            },
        )
    };
    let took_fds = matches!(&outcome, StartupAdoption::Taken { names, .. } if !names.is_empty());
    if INHERITED.set(store).is_err() && took_fds {
        // An accessor ran before adoption and created an empty store; the fds
        // just taken would be dropped (closed) with `store`. Say so loudly.
        eprintln!(
            "listeners: an inherited-listener accessor ran before adoption; the inherited \
             fds were closed and every listener will be bound"
        );
    }
    let _ = STARTUP_ADOPTION.set(outcome);
}

/// Whether the environment addressed inherited fds to this process at startup
/// — taken (whatever their names) or present but unreadable. When true a
/// socket unit may own the API port, so the API must never fall back to
/// another port: a runner on `port + 1` is one no client dials.
pub fn inherited_at_startup() -> bool {
    matches!(
        STARTUP_ADOPTION.get(),
        Some(StartupAdoption::Taken { .. }) | Some(StartupAdoption::Refused(_))
    )
}

/// Names of inherited fds still held (not handed to any listener).
pub fn unclaimed_inherited() -> Vec<String> {
    inherited().names()
}

/// Log what [`adopt_inherited_at_startup`] did, once logging exists.
pub fn log_startup_adoption() {
    match STARTUP_ADOPTION.get() {
        None | Some(StartupAdoption::NothingInherited) => {
            tracing::debug!("listeners: no inherited listening sockets (LISTEN_FDS absent)")
        }
        Some(StartupAdoption::Taken { names, not_open }) => {
            if !names.iter().any(|n| n == API_LISTENER_NAME) {
                tracing::warn!(
                    ?names,
                    "listeners: inherited sockets arrived but none is named `api` — set \
                     FileDescriptorName=api in the socket unit; the API will try to bind its \
                     port itself, which FAILS while the unit holds it, and will NOT fall back \
                     to another port"
                );
            }
            if not_open.is_empty() {
                tracing::info!(?names, "listeners: took inherited listening sockets");
            } else {
                tracing::warn!(
                    ?names,
                    ?not_open,
                    "listeners: took inherited listening sockets; some passed fds were not open"
                );
            }
        }
        Some(StartupAdoption::Refused(e)) => tracing::warn!(
            error = %e,
            "listeners: LISTEN_* variables present but unreadable — nothing adopted, every \
             listener will be bound"
        ),
        Some(StartupAdoption::Unsupported) => tracing::warn!(
            "listeners: LISTEN_* variables present but this OS has no adoption path — every \
             listener will be bound"
        ),
    }
}

/// The `/health` `listenerInheritance` block.
///
/// `supported` is whether this build can adopt an inherited listener on this
/// OS — the field the socket-unit installer reads to refuse a build that would
/// fail to bind under a socket unit. `api` is `"inherited"` / `"bound"`, or
/// `null` before the API listener was acquired (UNKNOWN, never "bound").
pub fn health_json() -> serde_json::Value {
    serde_json::json!({
        "supported": cfg!(unix),
        "api": origin_of(API_LISTENER_NAME).map(ListenerOrigin::as_str),
    })
}

/// What `GET /livez` said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivezVerdict {
    /// `200`: a serving runner.
    Live,
    /// `503` with `RUNNER_BOOTING`: a runner that holds its listener and is
    /// building its routes.
    Booting,
    /// Some other HTTP status: something answers HTTP, but not as a runner.
    OtherStatus(u16),
    /// The connect itself failed inside the budget (refused, unreachable, or
    /// timed out before the handshake): nothing listens on the port.
    NoAnswer,
    /// The connect succeeded but nothing — or only a strict prefix of
    /// `HTTP/` — came back inside the budget (silence or a reset). Bytes that
    /// are not HTTP are [`Self::NoAnswer`] instead: they prove something other
    /// than a runner holds the port. Two different
    /// worlds look like this and a probe cannot tell them apart: a socket unit
    /// holding a DEAD runner's port (the kernel completes the handshake from
    /// the backlog), and a LIVE runner too loaded to answer in time.
    Silent,
}

impl LivezVerdict {
    /// A runner process is there (serving or booting): the positive answer.
    pub fn runner_alive(self) -> bool {
        matches!(self, Self::Live | Self::Booting)
    }

    /// Whether a runner MAY be there: everything except a failed connect and
    /// an HTTP answer that is not a runner's. A caller that DESTROYS something
    /// on a "dead" verdict (a sibling's credential file, its registration)
    /// uses this, so a live runner that was merely slow ([`Self::Silent`]) is
    /// never taken for dead — a false "dead" there costs a live sibling its
    /// credentials, while a false "alive" keeps a leftover for as long as the
    /// port stays silent (bounded in practice by whatever holds the port: a
    /// socket unit activates its runner on the probe's own connect).
    pub fn may_be_alive(self) -> bool {
        matches!(self, Self::Live | Self::Booting | Self::Silent)
    }
}

/// Ask `GET /livez` at `addr` and classify the answer, within `budget` in
/// total (connect, write and read). BLOCKING: call it from a blocking thread.
pub fn probe_livez(addr: SocketAddr, budget: Duration) -> LivezVerdict {
    let deadline = Instant::now() + budget;
    let remaining = || {
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1))
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, remaining()) else {
        return LivezVerdict::NoAnswer;
    };
    // From here on the port accepted the connect: no answer is `Silent`.
    let request =
        format!("GET /livez HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: */*\r\n\r\n");
    if stream.set_write_timeout(Some(remaining())).is_err()
        || stream.write_all(request.as_bytes()).is_err()
    {
        return LivezVerdict::Silent;
    }
    // Read until the server closes (Connection: close), the deadline passes,
    // or the cap is reached — the verdict needs the status line and, for a
    // 503, the envelope's code.
    const CAP: usize = 16 * 1024;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        if Instant::now() >= deadline || buf.len() >= CAP {
            break;
        }
        if stream.set_read_timeout(Some(remaining())).is_err() {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    match classify_livez_response(&buf) {
        // Nothing, or a reply cut off inside `HTTP/`: silence, not evidence.
        LivezVerdict::NoAnswer if b"HTTP/".starts_with(&buf) => LivezVerdict::Silent,
        // Bytes that are not HTTP: positively not a runner.
        verdict => verdict,
    }
}

/// Classify raw response bytes (the pure half of [`probe_livez`]).
pub fn classify_livez_response(raw: &[u8]) -> LivezVerdict {
    let text = String::from_utf8_lossy(raw);
    let Some(status_line) = text.lines().next() else {
        return LivezVerdict::NoAnswer;
    };
    let mut parts = status_line.split_whitespace();
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return LivezVerdict::NoAnswer;
    };
    if !version.starts_with("HTTP/") {
        return LivezVerdict::NoAnswer;
    }
    let Ok(code) = code.parse::<u16>() else {
        return LivezVerdict::NoAnswer;
    };
    match code {
        200 => LivezVerdict::Live,
        503 if text.contains(RUNNER_BOOTING_CODE) => LivezVerdict::Booting,
        other => LivezVerdict::OtherStatus(other),
    }
}

/// Whether a runner MAY be on `127.0.0.1:port` ([`LivezVerdict::may_be_alive`]):
/// false only when the connect fails or something other than a runner answers
/// HTTP there. For callers that reap or deregister on `false`. A silent port is
/// kept: under a socket unit that may be a dead runner's port, but the runner
/// that owns that port reaps its own leftovers when it comes back, and a
/// sibling must never reap a live-but-loaded runner's files.
pub fn loopback_runner_alive(port: u16, budget: Duration) -> bool {
    probe_livez(SocketAddr::from(([127, 0, 0, 1], port)), budget).may_be_alive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    #[test]
    fn parse_reads_named_fds_addressed_to_us() {
        let got = parse_listen_env(Some("42"), Some("2"), Some("api:other"), 42)
            .unwrap()
            .unwrap();
        assert_eq!(got.start, SD_LISTEN_FDS_START);
        let fds: Vec<_> = got.fds().collect();
        assert_eq!(fds, vec![(3, "api"), (4, "other")]);
    }

    #[test]
    fn parse_names_default_to_unknown() {
        let got = parse_listen_env(Some("7"), Some("2"), None, 7)
            .unwrap()
            .unwrap();
        assert_eq!(got.names, vec![UNKNOWN_FD_NAME, UNKNOWN_FD_NAME]);
    }

    #[test]
    fn parse_ignores_fds_addressed_to_another_pid_or_absent() {
        assert_eq!(
            parse_listen_env(Some("41"), Some("1"), Some("api"), 42),
            Ok(None)
        );
        assert_eq!(parse_listen_env(None, Some("1"), Some("api"), 42), Ok(None));
        assert_eq!(parse_listen_env(Some("42"), None, None, 42), Ok(None));
        assert_eq!(parse_listen_env(Some("42"), Some("0"), None, 42), Ok(None));
    }

    #[test]
    fn parse_refuses_malformed_values() {
        assert!(matches!(
            parse_listen_env(Some("x"), Some("1"), None, 1),
            Err(ListenEnvError::BadPid(_))
        ));
        assert!(matches!(
            parse_listen_env(Some("1"), Some("-1"), None, 1),
            Err(ListenEnvError::BadCount(_))
        ));
        assert_eq!(
            parse_listen_env(Some("1"), Some("2"), Some("api"), 1),
            Err(ListenEnvError::NameCountMismatch { fds: 2, names: 1 })
        );
    }

    /// The encoding a self-managed predecessor sets for its successor is read
    /// back by the same parser systemd's variables go through.
    #[test]
    fn listen_env_for_round_trips_through_the_parser() {
        let env: HashMap<&str, String> = listen_env_for(99, &["api", "aux"]).into_iter().collect();
        let got = parse_listen_env(
            env.get(ENV_LISTEN_PID).map(String::as_str),
            env.get(ENV_LISTEN_FDS).map(String::as_str),
            env.get(ENV_LISTEN_FDNAMES).map(String::as_str),
            99,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got.names, vec!["api", "aux"]);
    }

    #[test]
    fn classify_livez_answers() {
        assert_eq!(
            classify_livez_response(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok"),
            LivezVerdict::Live
        );
        assert_eq!(
            classify_livez_response(
                b"HTTP/1.1 503 Service Unavailable\r\nretry-after: 2\r\n\r\n{\"code\":\"RUNNER_BOOTING\"}"
            ),
            LivezVerdict::Booting
        );
        assert_eq!(
            classify_livez_response(b"HTTP/1.1 503 Service Unavailable\r\n\r\n"),
            LivezVerdict::OtherStatus(503)
        );
        assert_eq!(classify_livez_response(b""), LivezVerdict::NoAnswer);
        assert_eq!(
            classify_livez_response(b"SSH-2.0-OpenSSH\r\n"),
            LivezVerdict::NoAnswer
        );
    }

    /// A port that accepts the connect and never answers — what a socket unit
    /// holding a dead runner's port, and a live runner too loaded to answer,
    /// both look like — is `Silent`: not a positive answer, but kept by the
    /// destructive callers.
    #[test]
    fn a_listener_that_never_answers_is_silent_and_kept() {
        let silent = std::net::TcpListener::bind(loopback(0)).unwrap();
        let port = silent.local_addr().unwrap().port();
        // The kernel completes the handshake from the backlog; nobody accepts.
        assert!(TcpStream::connect(loopback(port)).is_ok());
        let started = Instant::now();
        let verdict = probe_livez(loopback(port), Duration::from_millis(300));
        assert_eq!(verdict, LivezVerdict::Silent);
        // Not a positive answer...
        assert!(!verdict.runner_alive());
        // ...but not proof of death either: a loaded live runner looks the same.
        assert!(verdict.may_be_alive());
        assert!(loopback_runner_alive(port, Duration::from_millis(300)));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "budget not honoured"
        );
        drop(silent);
    }

    /// A port whose holder answers with bytes that are not HTTP is positively
    /// not a runner: not kept as `Silent`.
    #[test]
    fn a_non_http_reply_is_not_a_runner() {
        let server = std::net::TcpListener::bind(loopback(0)).unwrap();
        let addr = server.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = server.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = s.read(&mut req);
            s.write_all(b"-ERR unknown command\r\n").unwrap();
        });
        let verdict = probe_livez(addr, Duration::from_secs(5));
        handle.join().unwrap();
        assert_eq!(verdict, LivezVerdict::NoAnswer);
        assert!(!verdict.may_be_alive());
    }

    /// A closed port is not alive either.
    #[test]
    fn a_refused_port_is_not_alive() {
        let port = {
            let l = std::net::TcpListener::bind(loopback(0)).unwrap();
            l.local_addr().unwrap().port()
        };
        assert!(!loopback_runner_alive(port, Duration::from_millis(300)));
    }

    /// A served `/livez` and a booting `503 RUNNER_BOOTING` are both alive.
    #[test]
    fn served_and_booting_livez_are_alive() {
        for (response, expected) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                LivezVerdict::Live,
            ),
            (
                "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 2\r\nConnection: close\r\n\r\n{\"success\":false,\"code\":\"RUNNER_BOOTING\"}",
                LivezVerdict::Booting,
            ),
        ] {
            let server = std::net::TcpListener::bind(loopback(0)).unwrap();
            let addr = server.local_addr().unwrap();
            let handle = std::thread::spawn(move || {
                let (mut s, _) = server.accept().unwrap();
                let mut req = [0u8; 1024];
                let n = s.read(&mut req).unwrap();
                let req = String::from_utf8_lossy(&req[..n]).to_string();
                s.write_all(response.as_bytes()).unwrap();
                req
            });
            let verdict = probe_livez(addr, Duration::from_secs(5));
            let req = handle.join().unwrap();
            assert!(req.starts_with("GET /livez HTTP/1.1\r\n"), "{req}");
            assert!(req.contains(&format!("Host: {addr}")), "{req}");
            assert_eq!(verdict, expected);
            assert!(verdict.runner_alive());
        }
    }

    #[cfg(unix)]
    mod unix {
        use super::super::*;
        use super::loopback;
        use std::os::fd::IntoRawFd;

        fn env_for(pid: u32, names: &str, count: usize) -> HashMap<String, String> {
            HashMap::from([
                (ENV_LISTEN_PID.to_string(), pid.to_string()),
                (ENV_LISTEN_FDS.to_string(), count.to_string()),
                (ENV_LISTEN_FDNAMES.to_string(), names.to_string()),
            ])
        }

        /// A pre-bound listener handed over through the env protocol is
        /// adopted (same port, still listening, non-blocking, close-on-exec),
        /// and the three variables are gone afterwards.
        #[test]
        fn a_listener_passed_through_the_env_is_adopted_and_the_env_cleared() {
            let pre = bind_reuseaddr(loopback(0)).unwrap();
            let addr = pre.local_addr().unwrap();
            let fd = pre.into_raw_fd();

            let mut env = env_for(4242, API_LISTENER_NAME, 1);
            let (store, outcome) = InheritedListeners::from_env(&mut env, 4242, fd);
            assert_eq!(
                outcome,
                StartupAdoption::Taken {
                    names: vec![API_LISTENER_NAME.to_string()],
                    not_open: vec![]
                }
            );
            for key in LISTEN_ENV_KEYS {
                assert!(!env.contains_key(key), "{key} not cleared");
            }
            // SAFETY: fcntl on our own open fd.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(
                flags & libc::FD_CLOEXEC != 0,
                "inherited fd not close-on-exec"
            );

            let acquired = adopt_or_bind_from(&store, API_LISTENER_NAME, addr, |_| {
                panic!("an inherited listener must not be re-bound")
            })
            .unwrap();
            assert_eq!(acquired.origin, ListenerOrigin::Inherited);
            assert_eq!(acquired.listener.local_addr().unwrap(), addr);
            // Non-blocking: no pending connection reads WouldBlock, not a hang.
            let err = acquired.listener.accept().unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
            // And it is the real socket: a client connect is accepted on it.
            let _client = TcpStream::connect(addr).unwrap();
            acquired.listener.set_nonblocking(false).unwrap();
            assert!(acquired.listener.accept().is_ok());
            assert!(!store.contains(API_LISTENER_NAME));
        }

        /// Variables addressed to another pid adopt nothing and are still
        /// cleared, so a child never inherits them.
        #[test]
        fn another_pids_fds_are_not_taken_but_the_env_is_cleared() {
            let mut env = env_for(1, API_LISTENER_NAME, 1);
            let (store, outcome) = InheritedListeners::from_env(&mut env, 2, 1_000_000);
            assert_eq!(outcome, StartupAdoption::NothingInherited);
            assert!(store.names().is_empty());
            assert!(env.is_empty());
        }

        /// An fd number the protocol names but that is not open is skipped,
        /// never wrapped.
        #[test]
        fn a_named_fd_that_is_not_open_is_skipped() {
            let mut env = env_for(5, API_LISTENER_NAME, 1);
            // Far above any fd a test process holds.
            let (store, outcome) = InheritedListeners::from_env(&mut env, 5, 900_000);
            assert_eq!(
                outcome,
                StartupAdoption::Taken {
                    names: vec![],
                    not_open: vec![900_000]
                }
            );
            assert!(!store.contains(API_LISTENER_NAME));
        }

        /// An inherited fd bound to a different address than expected is
        /// rejected, closed, and the expected address is bound instead.
        #[test]
        fn a_listener_on_the_wrong_address_is_rejected_and_the_address_bound() {
            let pre = bind_reuseaddr(loopback(0)).unwrap();
            let fd = pre.into_raw_fd();
            let mut env = env_for(9, API_LISTENER_NAME, 1);
            let (store, _) = InheritedListeners::from_env(&mut env, 9, fd);
            let wanted = bind_reuseaddr(loopback(0)).unwrap();
            let wanted_addr = wanted.local_addr().unwrap();
            drop(wanted);
            let acquired =
                adopt_or_bind_from(&store, API_LISTENER_NAME, wanted_addr, bind_reuseaddr).unwrap();
            assert_eq!(acquired.origin, ListenerOrigin::Bound);
            assert_eq!(acquired.listener.local_addr().unwrap(), wanted_addr);
            assert!(!store.contains(API_LISTENER_NAME));
        }

        /// A connected (non-listening) stream socket is refused.
        #[test]
        fn a_socket_that_is_not_listening_is_rejected() {
            let server = std::net::TcpListener::bind(loopback(0)).unwrap();
            let client = TcpStream::connect(server.local_addr().unwrap()).unwrap();
            let local = client.local_addr().unwrap();
            let fd = client.into_raw_fd();
            let mut env = env_for(3, API_LISTENER_NAME, 1);
            let (store, _) = InheritedListeners::from_env(&mut env, 3, fd);
            let err = store.take(API_LISTENER_NAME, local).unwrap_err();
            assert!(err.to_string().contains("not listening"), "{err}");
        }
    }

    #[test]
    fn health_json_reports_support_and_origin() {
        let v = health_json();
        assert_eq!(v["supported"], serde_json::json!(cfg!(unix)));
        assert!(v.get("api").is_some());
        record_origin("health-json-test", ListenerOrigin::Inherited);
        assert_eq!(
            origin_of("health-json-test"),
            Some(ListenerOrigin::Inherited)
        );
    }
}
