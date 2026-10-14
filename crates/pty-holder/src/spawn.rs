//! The RUNNER side of a holder's birth: how `qontinui-pty-holder` is spawned
//! so that it outlives the runner, and how a failed spawn is torn down without
//! signalling anything it cannot prove is its own.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2. Every requirement here was found by the Phase 0 spike's six review
//! rounds and recorded in the plan's "what it hands Phase 2" blockquote (the
//! spike itself was deleted in Phase 2 once these helpers replaced its
//! copies). It lives in this LIBRARY (not in src-tauri)
//! so the holder crate's own integration tests can drive it against the real
//! holder binary on every CI leg, Windows included — the only place its
//! `cfg(windows)` arm executes at all.
//!
//! ## Routes
//!
//! - **Linux, runner inside a systemd unit's cgroup** — [`ResolvedRoute::Scope`]:
//!   `systemd-run --user --scope --collect --unit=qontinui-pty-holder-<pane>-<nonce>
//!   -- <holder> …`. `setsid()` does not leave a cgroup, and a runner under a
//!   unit with `KillMode=control-group` (this fleet's Linux boxes) takes every
//!   holder in its cgroup with it on stop, restart or crash; a transient scope
//!   of the holder's own does not. **It does not survive** a restart of the
//!   user manager itself, or logout without linger — the scope is a unit of
//!   that same manager.
//! - **Otherwise** — [`ResolvedRoute::Plain`]: a direct spawn; the holder
//!   `setsid()`s itself (`crate::startup`).
//! - **Fallback is visible, never silent.** An `Auto` scope attempt that
//!   produces no ready line falls back to `Plain`; a `Plain` holder inside a
//!   detected unit cgroup is returned with
//!   [`SpawnedHolder::unprotected`] = `Some(Unprotected::Cgroup { .. })` — the
//!   pane WILL die with that unit, and the caller must surface it rather than
//!   claim survival. A forced `Scope` never falls back.
//! - **Windows** — [`ResolvedRoute::Windows`]:
//!   `qontinui_runner_win32::holder_spawn::spawn_holder`, which decides plain
//!   `DETACHED_PROCESS` vs `CREATE_BREAKAWAY_FROM_JOB` from `IsProcessInJob`
//!   (plan D4 as corrected 2026-09-27). When the enclosing job refuses
//!   breakaway, the spawn is re-issued through WMI `Win32_Process.Create`
//!   (`holder_spawn::spawn_holder_wmi`, Phase 3) with `--report`, and the ready
//!   line is read from the pane's report file instead of a pipe. The route
//!   actually taken is [`SpawnedHolder::windows_route`]. Breakaway happens per
//!   holder spawn (D13): each pane's holder decides for itself.
//!
//! ## The new-tab budget
//!
//! Under D13 a holder spawn is on the path of every new tab, and the WMI
//! route is far slower than `CreateProcess` (the Phase 0 measurement it was
//! owed is UNRUN on Windows). So every spawn MEASURES itself —
//! [`SpawnedHolder::ready_after`], spawn to ready line, whatever the route —
//! and [`NEW_TAB_SPAWN_BUDGET`] is what a caller compares it with: over budget
//! is logged with the route, never silently absorbed. The hard bound remains
//! [`SpawnRequest::report_timeout`].
//!
//! ## What the spawner guarantees
//!
//! - **The holder executable is passed in** ([`SpawnRequest::holder_exe`]),
//!   resolved once by the caller (beside the runner's own executable). Never
//!   `current_exe()` at spawn time — that breaks after an in-place rebuild.
//! - **The holder is never its own process-group leader**: no
//!   `process_group(0)`, so its `setsid()` succeeds and the process this
//!   spawner holds IS the holder (the holder refuses rather than re-exec'ing).
//! - **The holder's own environment is an allowlist** ([`holder_env`]): what it
//!   needs to run and, on the scope route, to reach the user manager. The
//!   CHILD's environment travels in the spec, complete (`crate::spec`).
//! - **Every wait is deadline-bounded**: the ready line
//!   ([`SpawnRequest::report_timeout`]), a failed holder's exit
//!   ([`FAILURE_WAIT`]), its stderr, and `systemctl --user stop`
//!   ([`SYSTEMCTL_DEADLINE`]).
//! - **Teardown signals only verified pids**: pids come from a `holder_ready`
//!   line only ([`parse_ready_line`]), each in `2..=i32::MAX`; the reported
//!   holder pid must be the process actually spawned; the PTY child is
//!   signalled only once `/proc` confirms its parent is that holder and it
//!   leads its own session ([`verify_pty_child`], Linux only — elsewhere it is
//!   never signalled explicitly, and the holder's death hangs up its PTY).
//! - **Exited holders are reaped** — [`SpawnedHolder::reap_in_background`]
//!   hands the process to one shared reaper thread that polls `try_wait`.
//! - **The spec never outlives the attempt**: written before each attempt,
//!   removed after it (the holder unlinks it itself on success).
//!
//! No async runtime: the runner calls this from a blocking context.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::pane::{spec_path, PaneId};
use crate::spec::{write_spec, ChildSpec};

/// Prefix of every transient scope unit a holder is placed in.
pub const HOLDER_UNIT_PREFIX: &str = "qontinui-pty-holder-";

/// Default bound on the holder's ready line. The dedicated holder binary
/// reports in milliseconds (the spike's re-exec'd runner took ~72 ms); this is
/// headroom for a loaded box and a slow `systemd-run`.
pub const DEFAULT_REPORT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a holder that printed an error (or nothing) gets to exit before it
/// is killed.
pub const FAILURE_WAIT: Duration = Duration::from_secs(3);

/// Bound on `systemctl --user stop --no-block`: `--no-block` only enqueues the
/// stop job, but the D-Bus call itself can still hang on a wedged manager.
pub const SYSTEMCTL_DEADLINE: Duration = Duration::from_secs(5);

/// How often the shared reaper polls the holders it adopted.
pub const REAP_INTERVAL: Duration = Duration::from_secs(2);

/// What a new tab may spend on its holder's spawn before the delay is worth
/// a warning: spawn to ready line, on any route. Not a deadline — a slower
/// spawn still succeeds (the hard bound is the report timeout) — but the
/// number the WMI route's latency is held against (plan D13 / Phase 3).
pub const NEW_TAB_SPAWN_BUDGET: Duration = Duration::from_secs(2);

/// How often a spawner reading a holder's report FILE (the WMI route) looks
/// for it.
pub const REPORT_FILE_POLL: Duration = Duration::from_millis(20);

/// Read one line on a helper thread, giving up after `timeout`. `None` means
/// the deadline passed (the helper thread stays parked on the pipe until its
/// writer goes away, which the caller's teardown then causes). The line is
/// returned with its trailing newline trimmed; EOF with nothing is `Some("")`.
pub fn read_line_within<R: Read + Send + 'static>(reader: R, timeout: Duration) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pty-holder-report".into())
        .spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(reader).read_line(&mut line);
            let _ = tx.send(line);
        })
        .ok()?;
    rx.recv_timeout(timeout)
        .ok()
        .map(|l| l.trim_end().to_string())
}

/// Read to EOF on a helper thread, giving up after `timeout`.
pub fn read_all_within<R: Read + Send + 'static>(
    mut reader: R,
    timeout: Duration,
) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pty-holder-stderr".into())
        .spawn(move || {
            let mut all = String::new();
            let _ = reader.read_to_string(&mut all);
            let _ = tx.send(all);
        })
        .ok()?;
    rx.recv_timeout(timeout).ok()
}

/// True when a `/proc/<pid>/cgroup` body places the process inside a systemd
/// unit whose stop the runner itself can trigger — its own service (system or
/// `app.slice` user service) or a launcher's scope (`tmux-spawn-*.scope`,
/// `app-*.scope`). Such a unit's stop or crash-restart kills everything in its
/// cgroup under the default `KillMode=control-group`, `setsid()` or not.
///
/// Only the DEEPEST unit component counts — that is the unit the process is
/// actually in; the ones above it are ancestors. Excluded when deepest:
/// - `user@<uid>.service` and its `init.scope`: the user manager itself. A
///   scope route cannot escape it (the scope lives under it too), so claiming
///   protection there would be false.
/// - `session-<n>.scope`: a login session. The runner never stops it; what
///   logout does to it is logind policy (`KillUserProcesses`), which a
///   transient user scope does not change either.
pub fn cgroup_is_in_systemd_unit(cgroup_file: &str) -> bool {
    cgroup_file.lines().any(|line| {
        let Some(path) = line.splitn(3, ':').nth(2) else {
            return false;
        };
        let Some(unit) = path
            .split('/')
            .rfind(|c| c.ends_with(".service") || c.ends_with(".scope"))
        else {
            return false;
        };
        let user_manager = unit.starts_with("user@") && unit.ends_with(".service");
        let session = unit.starts_with("session-") && unit.ends_with(".scope");
        let manager_init = unit == "init.scope";
        !(user_manager || session || manager_init)
    })
}

/// `systemd-run` on `$PATH`, if any.
pub fn find_systemd_run() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("systemd-run"))
        .find(|p| p.is_file())
}

/// Whether a user systemd instance is reachable: its private socket (or the
/// user bus) exists under `$XDG_RUNTIME_DIR`. Presence, not proof: an `Auto`
/// scope spawn that fails anyway falls back, visibly.
pub fn user_systemd_reachable() -> bool {
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return false;
    };
    let rt = PathBuf::from(rt);
    rt.join("systemd/private").exists() || rt.join("bus").exists()
}

/// `systemd-run --user --scope --quiet --collect --unit=<unit> -- <exe> <args>`.
/// In scope mode `systemd-run` registers ITSELF in the new scope and then execs
/// the command, so the holder (and the PTY child it forks) live in a cgroup of
/// their own, outside the spawner's unit — and the holder keeps systemd-run's
/// pid, so the process the spawner holds is the holder.
pub fn scope_command(systemd_run: &Path, unit: &str, exe: &Path, args: &[OsString]) -> Command {
    // console-ok: systemd-run exists only on Linux; the scope route is refused on Windows.
    let mut cmd = Command::new(systemd_run);
    cmd.args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg("--")
        .arg(exe)
        .args(args);
    cmd
}

/// `v` as a pid this process may signal: `2..=i32::MAX`, so no parse can turn
/// into `kill(0)`, `kill(-1)` or a signal to init.
pub fn signalable_pid(v: u32) -> Option<i32> {
    (v > 1 && v <= i32::MAX as u32).then_some(v as i32)
}

/// `child`, but only if `/proc` confirms it is still the PTY child of `holder`:
/// its parent is `holder` and it leads its own session (portable-pty's child
/// `setsid()`s onto the PTY). `None` when the identity cannot be verified —
/// callers then rely on the holder's death (or the scope unit's stop) instead
/// of signalling a pid that may be recycled.
///
/// **Only confirms on Linux** (it reads `/proc`); elsewhere it always answers
/// `None`, so off Linux a teardown never signals the PTY child explicitly.
#[cfg(target_os = "linux")]
pub fn verify_pty_child(holder: i32, child: i32) -> Option<i32> {
    let raw = std::fs::read_to_string(format!("/proc/{child}/stat")).ok()?;
    // The comm field is parenthesised and may itself contain ')' or spaces, so
    // the fields we want start after the LAST ')'.
    let (_, rest) = raw.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ppid: i32 = f.get(1)?.parse().ok()?;
    let session: i32 = f.get(3)?.parse().ok()?;
    (ppid == holder && session == child).then_some(child)
}

/// Non-Linux: identity cannot be confirmed, so never a pid.
#[cfg(not(target_os = "linux"))]
pub fn verify_pty_child(_holder: i32, _child: i32) -> Option<i32> {
    None
}

/// `systemctl --user stop --no-block --quiet <unit>`, killed if it has not
/// returned within `deadline`. The unit's processes are then killed by the
/// manager asynchronously. `Err` says why the stop was not confirmed:
/// `systemctl` missing, failed, or out of time.
pub fn stop_scope_unit(unit: &str, deadline: Duration) -> Result<(), String> {
    // console-ok: only reached with a scope unit, i.e. on Linux.
    let mut systemctl = Command::new("systemctl")
        .args(["--user", "stop", "--no-block", "--quiet", unit])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("systemctl: {e}"))?;
    let end = Instant::now() + deadline;
    loop {
        match systemctl.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("systemctl {status}")),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = systemctl.kill();
                let _ = systemctl.wait();
                return Err(format!(
                    "systemctl gave no answer within {}ms",
                    deadline.as_millis()
                ));
            }
            Err(e) => {
                let _ = systemctl.kill();
                let _ = systemctl.wait();
                return Err(format!("systemctl wait: {e}"));
            }
        }
    }
}

/// The route a caller asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteRequest {
    /// Decided by the observables ([`HostFacts`]). Production uses this.
    Auto,
    /// A direct spawn (Linux/macOS: the holder `setsid()`s itself).
    Plain,
    /// Linux only: a transient systemd user scope. Never falls back.
    Scope,
    /// Windows only: WMI `Win32_Process.Create`, without first trying
    /// `CreateProcess`. What `Auto` falls back to inside a job that forbids
    /// breakaway; asked for directly only to exercise that route.
    Wmi,
}

/// A route, resolved for THIS OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedRoute {
    Plain,
    Scope,
    /// `qontinui_runner_win32::holder_spawn` decides plain vs breakaway.
    Windows,
}

impl ResolvedRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedRoute::Plain => "plain",
            ResolvedRoute::Scope => "scope",
            ResolvedRoute::Windows => "windows",
        }
    }
}

/// The observables a route decision is made from — gathered once by
/// [`HostFacts::probe`], passed explicitly so [`decide_route`] is testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostFacts {
    pub windows: bool,
    pub linux: bool,
    /// [`cgroup_is_in_systemd_unit`] on `/proc/self/cgroup`.
    pub in_unit_cgroup: bool,
    /// `systemd-run` on `$PATH` AND [`user_systemd_reachable`].
    pub scope_tooling: bool,
}

impl HostFacts {
    /// This box, now.
    pub fn probe() -> Self {
        let linux = cfg!(target_os = "linux");
        HostFacts {
            windows: cfg!(windows),
            linux,
            in_unit_cgroup: linux
                && std::fs::read_to_string("/proc/self/cgroup")
                    .map(|c| cgroup_is_in_systemd_unit(&c))
                    .unwrap_or(false),
            scope_tooling: linux && find_systemd_run().is_some() && user_systemd_reachable(),
        }
    }
}

/// A resolved route plus what the spawner must be told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDecision {
    pub route: ResolvedRoute,
    /// The holder will share a systemd unit's cgroup and die with that unit.
    pub unprotected_cgroup: bool,
    /// `Auto` chose this route, so a failed scope spawn may fall back.
    pub may_fall_back: bool,
}

/// Decide a route against `facts`. Refuses a route this OS cannot take rather
/// than silently substituting another.
pub fn decide_route(req: RouteRequest, facts: &HostFacts) -> Result<RouteDecision, String> {
    let decision = |route, may_fall_back| RouteDecision {
        route,
        unprotected_cgroup: route == ResolvedRoute::Plain && facts.in_unit_cgroup,
        may_fall_back,
    };
    if facts.windows {
        return match req {
            RouteRequest::Auto | RouteRequest::Plain | RouteRequest::Wmi => {
                Ok(decision(ResolvedRoute::Windows, false))
            }
            RouteRequest::Scope => Err("the scope route does not exist on Windows".into()),
        };
    }
    match req {
        RouteRequest::Wmi => Err("the WMI route exists only on Windows".into()),
        RouteRequest::Auto if facts.in_unit_cgroup && facts.scope_tooling => {
            Ok(decision(ResolvedRoute::Scope, true))
        }
        RouteRequest::Auto | RouteRequest::Plain => Ok(decision(ResolvedRoute::Plain, false)),
        RouteRequest::Scope if facts.linux => Ok(decision(ResolvedRoute::Scope, false)),
        RouteRequest::Scope => Err("the scope route exists only on Linux".into()),
    }
}

/// The holder's success report, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyLine {
    pub holder_pid: i32,
    pub child_pid: i32,
    pub pane_id: String,
    /// The endpoint: a socket path (Unix) or a pipe name (Windows). Last on
    /// the line, so it may contain spaces.
    pub endpoint: String,
}

/// Parse a `holder_ready holder_pid=<n> child_pid=<n> pane_id=<id> endpoint=<…>`
/// line. ONLY that line yields pids — never a `holder_error=` line, which can
/// embed arbitrary text — and only real, signalable ones
/// ([`signalable_pid`]).
pub fn parse_ready_line(line: &str) -> Option<ReadyLine> {
    let rest = line.strip_prefix("holder_ready ")?;
    let (fields, endpoint) = rest.split_once(" endpoint=")?;
    let field = |key: &str| {
        fields
            .split_whitespace()
            .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
    };
    Some(ReadyLine {
        holder_pid: signalable_pid(field("holder_pid")?.parse().ok()?)?,
        child_pid: signalable_pid(field("child_pid")?.parse().ok()?)?,
        pane_id: field("pane_id")?.to_string(),
        endpoint: endpoint.to_string(),
    })
}

/// The environment variables a holder PROCESS is given; everything else of
/// the runner's environment is withheld (the child's comes from the spec).
/// These are what the holder, `systemd-run` (reaching the user manager) and the
/// Windows process loader need.
pub const HOLDER_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    // Windows.
    "SystemRoot",
    "SystemDrive",
    "windir",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "ComSpec",
    "PATHEXT",
];

/// [`HOLDER_ENV_ALLOWLIST`] applied to this process's environment.
pub fn holder_env() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| {
            HOLDER_ENV_ALLOWLIST.iter().any(|a| {
                if cfg!(windows) {
                    k.eq_ignore_ascii_case(a)
                } else {
                    k == *a
                }
            })
        })
        .collect()
}

/// Everything [`spawn_holder`] needs.
#[derive(Debug, Clone, Copy)]
pub struct SpawnRequest<'a> {
    /// The `qontinui-pty-holder` executable, resolved once by the caller.
    pub holder_exe: &'a Path,
    /// The pane directory (absolute; namespaced per runner instance).
    pub pane_dir: &'a Path,
    pub pane_id: &'a PaneId,
    pub child: &'a ChildSpec,
    pub route: RouteRequest,
    /// Bound on the ready line; see [`DEFAULT_REPORT_TIMEOUT`].
    pub report_timeout: Duration,
}

/// Why a holder is NOT protected from something that will kill it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unprotected {
    /// The holder shares the runner's systemd unit cgroup and dies when that
    /// unit stops or crash-restarts. `fallback_reason` is set when an `Auto`
    /// scope attempt failed and this is its plain fallback.
    Cgroup { fallback_reason: Option<String> },
}

impl std::fmt::Display for Unprotected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unprotected::Cgroup {
                fallback_reason: None,
            } => f.write_str("unprotected=cgroup"),
            Unprotected::Cgroup {
                fallback_reason: Some(r),
            } => write!(f, "unprotected=cgroup fallback_from=scope reason={r}"),
        }
    }
}

/// Why no holder was spawned. Every variant leaves nothing behind: the
/// spawned process (if any) was killed and reaped, its scope unit stopped, and
/// the spec removed.
#[derive(Debug)]
pub enum SpawnError {
    /// The requested route does not exist here.
    Route(String),
    /// The spec could not be written.
    Spec(std::io::Error),
    /// The OS refused to start the process (or `systemd-run` is missing; or,
    /// on Windows, WMI `Win32_Process.Create` failed or reported a process
    /// that is not the holder).
    Spawn(String),
    /// The holder printed nothing within the report deadline.
    Silent {
        route: ResolvedRoute,
        reason: String,
    },
    /// The holder reported `holder_error=<kind> …`.
    HolderFailed { line: String, status: String },
    /// A success line whose `holder_pid` is not the process spawned: none of
    /// its pids are trusted.
    NotOurHolder { line: String, spawned_pid: u32 },
    /// Windows WMI route: a holder for this pane is (or may be) LIVE but could
    /// be neither held nor ended. A caller must NOT fall back to an
    /// in-process PTY: the pane's command would run twice.
    LiveHolderUnreachable(String),
}

impl SpawnError {
    /// The `<kind>` of a `holder_error=<kind>` report (`lock_held`, `spec`,
    /// `child`, `detach`, `start`, `usage`).
    pub fn holder_error_kind(&self) -> Option<&str> {
        match self {
            SpawnError::HolderFailed { line, .. } => line
                .strip_prefix("holder_error=")?
                .split_whitespace()
                .next(),
            _ => None,
        }
    }
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::Route(m) => write!(f, "route: {m}"),
            SpawnError::Spec(e) => write!(f, "spec: {e}"),
            SpawnError::Spawn(m) => write!(f, "spawn: {m}"),
            SpawnError::Silent { route, reason } => {
                write!(
                    f,
                    "holder printed nothing (route={}): {reason}",
                    route.as_str()
                )
            }
            SpawnError::HolderFailed { line, status } => {
                write!(f, "holder failed [{status}]: {line}")
            }
            SpawnError::LiveHolderUnreachable(m) => {
                write!(
                    f,
                    "a live holder for this pane could not be held or ended: {m}"
                )
            }
            SpawnError::NotOurHolder { line, spawned_pid } => write!(
                f,
                "holder report does not name the spawned holder (pid {spawned_pid}): {line}"
            ),
        }
    }
}

impl std::error::Error for SpawnError {}

/// A holder that reported ready.
#[derive(Debug)]
pub struct SpawnedHolder {
    pub holder_pid: u32,
    pub child_pid: u32,
    pub endpoint: String,
    pub route: ResolvedRoute,
    /// The scope unit, on [`ResolvedRoute::Scope`].
    pub unit: Option<String>,
    /// `Some` when this pane will NOT survive something the caller might
    /// assume it survives. Surface it; never drop it.
    pub unprotected: Option<Unprotected>,
    /// Windows: the `holder_spawn` route actually taken (plain / breakaway /
    /// WMI).
    #[cfg(windows)]
    pub windows_route: qontinui_runner_win32::holder_spawn::HolderSpawnRoute,
    /// Spawn to ready line, measured — what [`NEW_TAB_SPAWN_BUDGET`] is held
    /// against.
    pub ready_after: Duration,
    process: HolderProcess,
}

impl SpawnedHolder {
    /// The route as one word: `plain`, `scope`, or on Windows the
    /// `holder_spawn` route (`plain` / `breakaway` / `wmi`).
    pub fn route_name(&self) -> &'static str {
        #[cfg(windows)]
        {
            self.windows_route.as_str()
        }
        #[cfg(not(windows))]
        {
            self.route.as_str()
        }
    }

    /// Whether this spawn took longer than [`NEW_TAB_SPAWN_BUDGET`].
    pub fn over_new_tab_budget(&self) -> bool {
        self.ready_after > NEW_TAB_SPAWN_BUDGET
    }

    /// `Some(exit description)` once the holder has exited (a child is then
    /// reaped, here); `None` while it runs.
    pub fn try_reap(&mut self) -> std::io::Result<Option<String>> {
        self.process.try_wait()
    }

    /// Hand the holder to the shared reaper, which collects its exit status
    /// whenever it ends (a holder that outlives this runner is reparented and
    /// reaped by init instead). A WMI-spawned holder is not our child and
    /// leaves no zombie; its handle is simply closed. The usual end of a
    /// [`SpawnedHolder`].
    pub fn reap_in_background(self) {
        match self.process {
            HolderProcess::Child(child) => adopt_for_reaping(child),
            #[cfg(windows)]
            HolderProcess::Wmi(handle) => drop(handle),
            #[cfg(windows)]
            HolderProcess::Unheld { .. } => {}
        }
    }

    /// End this pane outright — for a caller that cannot use the holder it
    /// just spawned (the normal end of a pane is the protocol's `kill`).
    /// SIGKILLs the PTY child only if `/proc` verifies it, stops the scope
    /// unit (bounded), then kills and reaps the holder.
    pub fn teardown(mut self) {
        let ready = ReadyLine {
            holder_pid: signalable_pid(self.holder_pid).unwrap_or(0),
            child_pid: signalable_pid(self.child_pid).unwrap_or(0),
            pane_id: String::new(),
            endpoint: String::new(),
        };
        abandon(&mut self.process, self.unit.as_deref(), Some(&ready));
    }
}

/// The process a spawner holds for its holder.
#[derive(Debug)]
enum HolderProcess {
    /// Spawned by `CreateProcess` / `fork`: our child.
    Child(Child),
    /// Spawned through WMI: a verified handle, not our child.
    #[cfg(windows)]
    Wmi(qontinui_runner_win32::holder_spawn::WmiProcess),
    /// Spawned through WMI, held by PANE identity, not by handle: its report
    /// said ready and its endpoint answered as that pid
    /// (`settle_unheld_wmi_holder`). Ended through its verified lock.
    #[cfg(windows)]
    Unheld {
        pid: u32,
        pane_dir: PathBuf,
        pane_id: PaneId,
        /// Its ready line, already read from the report file.
        ready_line: Option<String>,
    },
}

impl HolderProcess {
    fn id(&self) -> u32 {
        match self {
            HolderProcess::Child(c) => c.id(),
            #[cfg(windows)]
            HolderProcess::Wmi(w) => w.id(),
            #[cfg(windows)]
            HolderProcess::Unheld { pid, .. } => *pid,
        }
    }

    /// `Some(exit description)` once it has exited.
    fn try_wait(&mut self) -> std::io::Result<Option<String>> {
        match self {
            HolderProcess::Child(c) => Ok(c.try_wait()?.map(|s| s.to_string())),
            #[cfg(windows)]
            HolderProcess::Wmi(w) => Ok(w.try_wait()?.map(|code| format!("exit code: {code}"))),
            // Alive while its lock is held; its exit code is not ours to see.
            #[cfg(windows)]
            HolderProcess::Unheld {
                pane_dir, pane_id, ..
            } => Ok(
                match crate::lock::PaneLock::try_acquire_existing(&crate::pane::lock_path(
                    pane_dir, pane_id,
                )) {
                    Ok(crate::lock::TryLock::Held) => None,
                    _ => Some("exited (unheld)".into()),
                },
            ),
        }
    }

    /// Kill and (for a child) reap.
    fn kill_and_reap(&mut self) {
        match self {
            HolderProcess::Child(c) => {
                let _ = c.kill();
                let _ = c.wait();
            }
            #[cfg(windows)]
            HolderProcess::Wmi(w) => {
                let _ = w.kill();
                let _ = w.wait_timeout(FAILURE_WAIT);
            }
            #[cfg(windows)]
            HolderProcess::Unheld {
                pid,
                pane_dir,
                pane_id,
                ..
            } => {
                let _ = crate::terminate::terminate_verified_holder(pane_dir, pane_id, *pid);
            }
        }
    }

    /// Whatever the process wrote to its stderr pipe, if it has one.
    fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        match self {
            HolderProcess::Child(c) => c.stderr.take(),
            #[cfg(windows)]
            HolderProcess::Wmi(_) | HolderProcess::Unheld { .. } => None,
        }
    }
}

/// Leave nothing behind, in this order:
/// 1. SIGKILL the PTY child named by `ready` — only when `ready`'s holder pid
///    is the process we spawned and [`verify_pty_child`] confirms the child;
///    unverifiable → no explicit signal (steps 2-3 still end it: the unit stop
///    kills its cgroup, and the holder's death hangs up its PTY).
/// 2. Stop the holder's scope unit, if any, within [`SYSTEMCTL_DEADLINE`].
/// 3. Kill and reap the holder process itself (on every route but WMI the
///    holder is our direct child, so this does not wait on the manager; a WMI
///    holder is killed through its verified handle).
fn abandon(process: &mut HolderProcess, unit: Option<&str>, ready: Option<&ReadyLine>) {
    #[cfg(unix)]
    if let Some(r) = ready {
        let ours = signalable_pid(process.id()) == Some(r.holder_pid);
        if let Some(child) = verify_pty_child(r.holder_pid, r.child_pid).filter(|_| ours) {
            // SAFETY: a plain signal to a pid in 2..=i32::MAX whose parent and
            // session were just verified against our own holder.
            unsafe {
                libc::kill(child, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = ready;
    if let Some(unit) = unit {
        // Best effort: the holder's kill below hangs up the PTY either way.
        let _ = stop_scope_unit(unit, SYSTEMCTL_DEADLINE);
    }
    process.kill_and_reap();
}

/// Why one attempt failed, before the fallback decision.
enum AttemptError {
    /// Printed nothing: the only failure an `Auto` scope falls back from.
    Silent(String),
    Other(SpawnError),
}

/// Spawn a holder for `req.pane_id` and wait for its ready line.
pub fn spawn_holder(req: &SpawnRequest<'_>) -> Result<SpawnedHolder, SpawnError> {
    let facts = HostFacts::probe();
    let decision = decide_route(req.route, &facts).map_err(SpawnError::Route)?;
    let mut route = decision.route;
    let mut fallback: Option<String> = None;
    loop {
        // A fresh spec per attempt: a scope attempt that got as far as the
        // holder consumed (and unlinked) the previous one.
        write_spec(req.pane_dir, req.pane_id, req.child).map_err(SpawnError::Spec)?;
        let attempt = attempt(req, route);
        // The holder unlinks its spec when it reads it; this covers every
        // path on which it did not.
        let _ = std::fs::remove_file(spec_path(req.pane_dir, req.pane_id));
        match attempt {
            Ok(mut h) => {
                if route == ResolvedRoute::Plain && facts.in_unit_cgroup {
                    h.unprotected = Some(Unprotected::Cgroup {
                        fallback_reason: fallback,
                    });
                }
                return Ok(h);
            }
            Err(AttemptError::Silent(reason))
                if route == ResolvedRoute::Scope && decision.may_fall_back =>
            {
                fallback = Some(
                    reason
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join("_")
                        .chars()
                        .take(300)
                        .collect(),
                );
                route = ResolvedRoute::Plain;
            }
            Err(AttemptError::Silent(reason)) => {
                return Err(SpawnError::Silent { route, reason });
            }
            Err(AttemptError::Other(e)) => return Err(e),
        }
    }
}

/// `--report` is added on the WMI route only (see the module docs).
fn holder_args(req: &SpawnRequest<'_>) -> Vec<OsString> {
    vec![
        "--pane-dir".into(),
        req.pane_dir.as_os_str().to_os_string(),
        "--pane-id".into(),
        req.pane_id.as_str().into(),
    ]
}

/// A unit name unique to this pane and this attempt. Pane ids are
/// `[A-Za-z0-9_-]`, all legal in a unit name.
fn unit_name(pane: &PaneId) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{HOLDER_UNIT_PREFIX}{}-{:x}-{nanos:x}",
        pane.as_str(),
        std::process::id()
    )
}

fn attempt(req: &SpawnRequest<'_>, route: ResolvedRoute) -> Result<SpawnedHolder, AttemptError> {
    let args = holder_args(req);
    let (mut cmd, unit) = match route {
        ResolvedRoute::Scope => {
            let systemd_run = find_systemd_run().ok_or_else(|| {
                AttemptError::Silent("scope route: systemd-run not on PATH".into())
            })?;
            let unit = unit_name(req.pane_id);
            (
                scope_command(&systemd_run, &unit, req.holder_exe, &args),
                Some(unit),
            )
        }
        ResolvedRoute::Plain | ResolvedRoute::Windows => {
            // console-ok: on Windows spawn_os hands this builder to runner-win32's
            // spawn_holder, which sets DETACHED_PROCESS (no console) itself.
            let mut cmd = Command::new(req.holder_exe);
            cmd.args(&args);
            (cmd, None)
        }
    };
    // NOT `process_group(0)`: a group leader cannot `setsid()` (module docs).
    cmd.env_clear()
        .envs(holder_env())
        // The pane dir, not the runner's cwd: a holder must not pin a
        // directory (a worktree) that may be removed while the pane lives.
        .current_dir(req.pane_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // systemd-run's stderr is the only account of WHY a scope failed; the
        // holder points fd 2 at /dev/null once it reports, so the pipe does
        // not stay pinned.
        .stderr(if unit.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let started = Instant::now();
    #[cfg(windows)]
    let (mut process, windows_route) = spawn_os(&mut cmd, req)?;
    #[cfg(not(windows))]
    let mut process = HolderProcess::Child(
        cmd.spawn()
            .map_err(|e| AttemptError::Other(SpawnError::Spawn(e.to_string())))?,
    );

    let line = match &mut process {
        HolderProcess::Child(child) => match child.stdout.take() {
            Some(out) => read_line_within(out, req.report_timeout),
            None => Some(String::new()),
        },
        #[cfg(windows)]
        HolderProcess::Wmi(_) => {
            read_report_file_within(req.pane_dir, req.pane_id, &mut process, req.report_timeout)
        }
        #[cfg(windows)]
        HolderProcess::Unheld { ready_line, .. } => ready_line.take(),
    };
    let ready_after = started.elapsed();
    let line = match line {
        Some(l) if !l.is_empty() => l,
        missed => {
            let what = if missed.is_none() {
                format!("no report within {}ms", req.report_timeout.as_millis())
            } else {
                "EOF with no report".to_string()
            };
            let reason = failure_reason(&mut process);
            abandon(&mut process, unit.as_deref(), None);
            return Err(AttemptError::Silent(format!("{what}: {reason}")));
        }
    };
    let Some(ready) = parse_ready_line(&line) else {
        let status = failure_reason(&mut process);
        abandon(&mut process, unit.as_deref(), None);
        return Err(AttemptError::Other(SpawnError::HolderFailed {
            line,
            status,
        }));
    };
    if signalable_pid(process.id()) != Some(ready.holder_pid) {
        let spawned_pid = process.id();
        abandon(&mut process, unit.as_deref(), None);
        return Err(AttemptError::Other(SpawnError::NotOurHolder {
            line,
            spawned_pid,
        }));
    }
    Ok(SpawnedHolder {
        holder_pid: ready.holder_pid as u32,
        child_pid: ready.child_pid as u32,
        endpoint: ready.endpoint,
        route,
        unit,
        unprotected: None,
        #[cfg(windows)]
        windows_route,
        ready_after,
        process,
    })
}

/// Windows: `CreateProcess` on the route `IsProcessInJob` decides, and WMI
/// when that is breakaway and the job refuses it (or when WMI was asked for).
/// The WMI holder gets `--report`: it has no stdout pipe to print on.
#[cfg(windows)]
fn spawn_os(
    cmd: &mut Command,
    req: &SpawnRequest<'_>,
) -> Result<
    (
        HolderProcess,
        qontinui_runner_win32::holder_spawn::HolderSpawnRoute,
    ),
    AttemptError,
> {
    use qontinui_runner_win32::holder_spawn::{
        spawn_holder, spawn_holder_wmi, HolderSpawnError, HolderSpawnRoute,
    };
    if req.route != RouteRequest::Wmi {
        match spawn_holder(cmd) {
            Ok((child, route)) => return Ok((HolderProcess::Child(child), route)),
            Err(HolderSpawnError::NeedsWmiFallback { .. }) => {}
            Err(other) => return Err(AttemptError::Other(SpawnError::Spawn(other.to_string()))),
        }
    }
    let report = crate::pane::report_path(req.pane_dir, req.pane_id);
    let _ = std::fs::remove_file(&report);
    cmd.arg("--report");
    match spawn_holder_wmi(cmd, req.report_timeout) {
        Ok(w) => Ok((HolderProcess::Wmi(w), HolderSpawnRoute::Wmi)),
        Err(e @ HolderSpawnError::WmiNotOurProcess { .. }) => {
            settle_unheld_wmi_holder(req, &report, &e.to_string())
                .map(|unheld| (unheld, HolderSpawnRoute::Wmi))
        }
        Err(e) => Err(AttemptError::Other(SpawnError::Spawn(e.to_string()))),
    }
}

/// The holder WMI made could not be held by a process handle. Most often it
/// failed fast (say `lock_held`) and exited before it could be opened — but it
/// may be LIVE (its handle refused on a false identity mismatch), and a live
/// holder must never be left running while the caller falls back to an
/// in-process PTY (two copies of one command). So, judged by the pane itself:
///
/// - its report says `holder_error=…` → [`SpawnError::HolderFailed`];
/// - its report says `holder_ready`, and the pane's endpoint answers the
///   handshake as that holder pid → accepted, held by pane identity
///   ([`HolderProcess::Unheld`]) instead of by handle;
/// - otherwise, if the pane's lock is held, its recorded holder is ended
///   through `terminate::terminate_verified_holder`; a holder that can be
///   neither held nor ended is [`SpawnError::LiveHolderUnreachable`] — never
///   a plain failure a caller would fall back from.
#[cfg(windows)]
fn settle_unheld_wmi_holder(
    req: &SpawnRequest<'_>,
    report: &Path,
    why: &str,
) -> Result<HolderProcess, AttemptError> {
    let deadline = Instant::now() + req.report_timeout;
    let line = loop {
        if let Some(line) = take_report_file(report).filter(|l| !l.is_empty()) {
            break Some(line);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(REPORT_FILE_POLL);
    };
    if let Some(line) = &line {
        if line.starts_with("holder_error=") {
            return Err(AttemptError::Other(SpawnError::HolderFailed {
                line: line.clone(),
                status: format!("exited before it could be opened ({why})"),
            }));
        }
        if let Some(ready) = parse_ready_line(line) {
            let answered =
                crate::client::connect(req.pane_dir, req.pane_id, Instant::now() + FAILURE_WAIT)
                    .is_ok_and(|c| {
                        signalable_pid(c.hello_ack().holder_pid) == Some(ready.holder_pid)
                    });
            if answered {
                return Ok(HolderProcess::Unheld {
                    pid: ready.holder_pid as u32,
                    pane_dir: req.pane_dir.to_path_buf(),
                    pane_id: req.pane_id.clone(),
                    ready_line: Some(line.clone()),
                });
            }
        }
    }
    let lock_file = crate::pane::lock_path(req.pane_dir, req.pane_id);
    match crate::lock::PaneLock::try_acquire_existing(&lock_file) {
        Ok(crate::lock::TryLock::Held) => {
            let Some(record) = crate::lock::read_record(&lock_file) else {
                return Err(AttemptError::Other(SpawnError::LiveHolderUnreachable(
                    format!("{why}; the pane's lock is held by an unidentified holder"),
                )));
            };
            let t = crate::terminate::terminate_verified_holder(
                req.pane_dir,
                req.pane_id,
                record.holder_pid,
            );
            if t.signalled {
                Err(AttemptError::Other(SpawnError::Spawn(format!(
                    "{why}; the unheld holder (pid {}) was ended: {}",
                    record.holder_pid, t.detail
                ))))
            } else {
                Err(AttemptError::Other(SpawnError::LiveHolderUnreachable(
                    format!(
                        "{why}; holder pid {} holds the pane and could not be ended: {}",
                        record.holder_pid, t.detail
                    ),
                )))
            }
        }
        // Lock free or gone: no holder is alive for this pane.
        _ => Err(AttemptError::Other(SpawnError::Spawn(format!(
            "{why}; {}",
            line.unwrap_or_else(|| "no report".into())
        )))),
    }
}

/// Read (and remove) a holder's report file: its first line, or `None` when
/// there is no such file yet.
#[cfg(windows)]
fn take_report_file(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_string(&mut text)
        .ok()?;
    let _ = std::fs::remove_file(path);
    Some(text.lines().next().unwrap_or("").to_string())
}

/// The WMI route's ready line: poll the pane's report file (written whole by
/// a rename) until it appears, the holder exits without it (`Some("")`, as
/// EOF on a pipe), or `timeout` passes (`None`). The file is removed once read.
#[cfg(windows)]
fn read_report_file_within(
    pane_dir: &Path,
    pane_id: &PaneId,
    process: &mut HolderProcess,
    timeout: Duration,
) -> Option<String> {
    let path = crate::pane::report_path(pane_dir, pane_id);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(line) = take_report_file(&path) {
            return Some(line);
        }
        if matches!(process.try_wait(), Ok(Some(_))) {
            // It may have renamed the file just before exiting.
            return Some(take_report_file(&path).unwrap_or_default());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(REPORT_FILE_POLL);
    }
}

/// Why a holder that printed nothing (or an error) failed, as one
/// whitespace-free token: its exit status and whatever reached stderr (on the
/// scope route, systemd-run's). Waits up to [`FAILURE_WAIT`] for the process
/// to exit, KILLS it if it has not, then reads stderr — itself bounded, since
/// a grandchild could still hold the pipe.
fn failure_reason(process: &mut HolderProcess) -> String {
    let deadline = Instant::now() + FAILURE_WAIT;
    let status = loop {
        match process.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                process.kill_and_reap();
                break "still-running(killed)".to_string();
            }
        }
    };
    let err = process
        .take_stderr()
        .map(|e| {
            read_all_within(e, Duration::from_secs(2))
                .unwrap_or_else(|| "<stderr read timed out>".into())
        })
        .unwrap_or_default();
    format!("{} [{status}]", err.trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .take(300)
        .collect()
}

/// Hand an exited-or-running holder process to the one shared reaper thread,
/// which polls `try_wait` every [`REAP_INTERVAL`] and drops each process once
/// it has been collected. A blocking `wait()` per holder would cost a thread
/// per pane and an untimed wait the runner's subprocess gate rejects; one
/// poller costs one thread for all of them.
pub fn adopt_for_reaping(process: Child) {
    static ADOPTED: OnceLock<Mutex<Vec<Child>>> = OnceLock::new();
    let first = ADOPTED.get().is_none();
    let list = ADOPTED.get_or_init(|| Mutex::new(Vec::new()));
    list.lock().unwrap_or_else(|p| p.into_inner()).push(process);
    if first {
        let _ = std::thread::Builder::new()
            .name("pty-holder-reaper".into())
            .spawn(move || loop {
                std::thread::sleep(REAP_INTERVAL);
                list.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .retain_mut(|c| matches!(c.try_wait(), Ok(None)));
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX_IN_UNIT: HostFacts = HostFacts {
        windows: false,
        linux: true,
        in_unit_cgroup: true,
        scope_tooling: true,
    };

    #[test]
    fn pty_holder_spawn_routes_resolve_per_os() {
        let f = LINUX_IN_UNIT;
        let auto = decide_route(RouteRequest::Auto, &f).unwrap();
        assert_eq!(auto.route, ResolvedRoute::Scope);
        assert!(auto.may_fall_back);
        assert!(!auto.unprotected_cgroup);
        let forced = decide_route(RouteRequest::Scope, &f).unwrap();
        assert!(!forced.may_fall_back, "a forced scope must never fall back");
        // Plain inside a unit is unprotected, and says so.
        assert!(
            decide_route(RouteRequest::Plain, &f)
                .unwrap()
                .unprotected_cgroup
        );
        // A detected unit with no usable user systemd: plain AND unprotected.
        let d = decide_route(
            RouteRequest::Auto,
            &HostFacts {
                scope_tooling: false,
                ..f
            },
        )
        .unwrap();
        assert_eq!(d.route, ResolvedRoute::Plain);
        assert!(d.unprotected_cgroup);
        // Outside any unit, plain is simply plain.
        let d = decide_route(
            RouteRequest::Auto,
            &HostFacts {
                in_unit_cgroup: false,
                ..f
            },
        )
        .unwrap();
        assert_eq!(
            (d.route, d.unprotected_cgroup),
            (ResolvedRoute::Plain, false)
        );

        let win = HostFacts {
            windows: true,
            linux: false,
            in_unit_cgroup: false,
            scope_tooling: false,
        };
        assert_eq!(
            decide_route(RouteRequest::Auto, &win).unwrap().route,
            ResolvedRoute::Windows
        );
        assert!(decide_route(RouteRequest::Scope, &win).is_err());
        assert_eq!(
            decide_route(RouteRequest::Wmi, &win).unwrap().route,
            ResolvedRoute::Windows
        );
        assert!(
            decide_route(RouteRequest::Wmi, &f).is_err(),
            "the WMI route exists only on Windows"
        );
        let mac = HostFacts {
            linux: false,
            in_unit_cgroup: false,
            scope_tooling: false,
            ..f
        };
        assert!(decide_route(RouteRequest::Scope, &mac).is_err());
    }

    #[test]
    fn pty_holder_spawn_cgroup_detection_reads_the_unit_component() {
        assert!(cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service\n"
        ));
        assert!(cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/tmux-spawn-x.scope\n"
        ));
        assert!(cgroup_is_in_systemd_unit(
            "12:cpu:/\n1:name=systemd:/system.slice/foo.service\n"
        ));
        assert!(!cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service\n"
        ));
        assert!(!cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/init.scope\n"
        ));
        assert!(!cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/session-3.scope\n"
        ));
        assert!(!cgroup_is_in_systemd_unit("0::/\n"));
        assert!(!cgroup_is_in_systemd_unit(""));
    }

    #[test]
    fn pty_holder_spawn_ready_line_yields_only_real_pids() {
        let r = parse_ready_line(
            "holder_ready holder_pid=100 child_pid=200 pane_id=p1 endpoint=/tmp/a b/p1.sock",
        )
        .unwrap();
        assert_eq!((r.holder_pid, r.child_pid), (100, 200));
        assert_eq!(r.pane_id, "p1");
        assert_eq!(
            r.endpoint, "/tmp/a b/p1.sock",
            "the endpoint may hold spaces"
        );
        // An error line never yields pids, whatever it embeds.
        assert_eq!(
            parse_ready_line(
                "holder_error=spec holder_ready holder_pid=5 child_pid=6 pane_id=p endpoint=x"
            ),
            None
        );
        for bad in [
            "holder_ready holder_pid=100 child_pid=0 pane_id=p endpoint=x",
            "holder_ready holder_pid=100 child_pid=1 pane_id=p endpoint=x",
            "holder_ready holder_pid=1 child_pid=200 pane_id=p endpoint=x",
            "holder_ready holder_pid=100 child_pid=2147483648 pane_id=p endpoint=x",
            "holder_ready holder_pid=100 child_pid=-1 pane_id=p endpoint=x",
            "holder_ready holder_pid=100 child_pid=200 pane_id=p",
            "",
        ] {
            assert_eq!(parse_ready_line(bad), None, "{bad}");
        }
        assert_eq!(signalable_pid(i32::MAX as u32), Some(i32::MAX));
        assert_eq!(signalable_pid(u32::MAX), None);
    }

    #[test]
    fn pty_holder_spawn_scope_command_puts_the_holder_in_its_own_unit() {
        let cmd = scope_command(
            Path::new("/usr/bin/systemd-run"),
            "qontinui-pty-holder-x",
            Path::new("/opt/qontinui-pty-holder"),
            &["--pane-dir".into(), "/d".into()],
        );
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        assert_eq!(
            args,
            [
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--unit=qontinui-pty-holder-x",
                "--",
                "/opt/qontinui-pty-holder",
                "--pane-dir",
                "/d"
            ]
            .map(std::ffi::OsStr::new)
        );
        let unit = unit_name(&PaneId::new("pane-1").unwrap());
        assert!(unit.starts_with("qontinui-pty-holder-pane-1-"), "{unit}");
        assert!(unit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }

    #[test]
    fn pty_holder_spawn_holder_env_is_an_allowlist() {
        let env = holder_env();
        for (k, _) in &env {
            assert!(
                HOLDER_ENV_ALLOWLIST
                    .iter()
                    .any(|a| k.eq_ignore_ascii_case(a)),
                "{k:?} is not allowlisted"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_spawn_verify_pty_child_refuses_an_unrelated_process() {
        let me = std::process::id() as i32;
        assert_eq!(verify_pty_child(2, me), None);
    }

    #[test]
    fn pty_holder_spawn_error_kind_is_read_from_the_report() {
        let e = SpawnError::HolderFailed {
            line: "holder_error=lock_held lock held by holder pid 9".into(),
            status: "exit status: 3".into(),
        };
        assert_eq!(e.holder_error_kind(), Some("lock_held"));
        assert_eq!(SpawnError::Route("x".into()).holder_error_kind(), None);
    }
}
