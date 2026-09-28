//! Phase 0 survival spike: `--pty-holder-spike` and `--pty-holder-spike-parent`.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 0 — the hard go/no-go. Everything later in the plan assumes that a
//! PTY held by a process the runner spawned outlives the runner. This module
//! is the smallest thing that can be killed to find out.
//!
//! ## `--pty-holder-spike [--report-file <path>] [-- <program> <args>…]`
//!
//! 1. Closes every inherited fd >= 3 and resets `SIGCHLD` to `SIG_DFL`, so
//!    nothing the spawner leaked (a socket, a pipe end, a lock) is pinned for
//!    the pane's lifetime and `child.wait()` works whatever the spawner set.
//! 2. Detaches from its spawner. Unix: `setsid()`, so the holder is in neither
//!    the spawner's session nor its process group — a hangup of the spawner's
//!    terminal, or a kill of its process group, does not reach it. A holder
//!    that already LEADS a process group (job-control shell, `process_group(0)`)
//!    cannot `setsid()`; it re-execs itself once with `setsid()` in `pre_exec`
//!    and relays the real holder's line — measured on merytshost, where a
//!    plain `cmd &` from the agent's bash hit exactly this.
//!    `setsid()` does NOT leave the spawner's **cgroup**: under a systemd
//!    service with `KillMode=control-group` a unit stop kills the holder anyway.
//!    That is the spawner's job to solve — see the `scope` route below.
//!    Windows: detachment is decided by the SPAWNER's creation flags
//!    ([`qontinui_runner_win32::holder_spawn`]), because a process cannot leave
//!    a job it is already in.
//! 3. Opens a PTY through `portable-pty` and spawns the child on it (default: a
//!    long `sleep` / `timeout`).
//! 4. Reports exactly ONE line — `holder_pid=<n> child_pid=<n>`, or
//!    `holder_error=<reason>`: first to `--report-file` when asked (atomically,
//!    via a per-pid temp name), then to stdout. A report-file failure is fatal
//!    (the child is killed, exit non-zero), never silently skipped. The file
//!    exists for spawners that cannot read our stdout (the WMI route, a
//!    systemd service).
//! 5. Unix: points stdin/stdout/stderr at `/dev/null`, so the holder stops
//!    holding the spawner's pipe — the spawner's reader sees EOF instead of
//!    being pinned open for the pane's lifetime. (Not about `SIGPIPE`: Rust
//!    already ignores it, and a late write would just fail with `EPIPE`.)
//! 6. Keeps the master open and serves nothing: one thread drains the master so
//!    the child never blocks on a full PTY buffer, and the main thread waits on
//!    the child. The holder exits with the child's exit code.
//!
//! It never ignores `SIGHUP`: dispositions set to `SIG_IGN` survive `exec`, so
//! doing so would make the CHILD immune to its own terminal's hangup — the
//! signal that must still end a pane whose holder dies.
//!
//! ## `--pty-holder-spike-parent [--route <R>] [--report-file <path>] [-- <program> <args>…]`
//!
//! The spawner stand-in the integration tests kill. It waits for one line on
//! stdin (so a test can put it in a job object before it spawns anything; EOF
//! counts), spawns a holder the way the runner will, relays the holder's line
//! with ` route=<resolved route>` (and ` holder_unit=<unit>` for `scope`)
//! appended — to stdout and to `--report-file` — and then sleeps until killed.
//!
//! Routes (`R`), resolved by [`resolve_route`]:
//! - `auto` — the observable decides. Linux: `scope` when this process is in a
//!   systemd unit's cgroup AND a user systemd answers, else `plain`. Windows:
//!   [`qontinui_runner_win32::holder_spawn::spawn_holder`] (`IsProcessInJob`).
//! - `plain` (Unix + Windows) — a direct spawn; the holder `setsid()`s itself.
//! - `scope` (Linux) — `systemd-run --user --scope --collect
//!   --unit=qontinui-pty-holder-<uuid> -- <exe> --pty-holder-spike …`: the
//!   holder gets its OWN transient scope, so stopping (or crash-restarting) the
//!   spawner's `KillMode=control-group` service does not reach it.
//! - `breakaway`, `wmi` (Windows) — see `holder_spawn`.
//!
//! No async runtime and no Tauri: `main()` dispatches here before either exists.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// argv[1] that selects the holder.
pub const SPIKE_FLAG: &str = "--pty-holder-spike";
/// argv[1] that selects the spawner stand-in.
pub const SPIKE_PARENT_FLAG: &str = "--pty-holder-spike-parent";
/// Prefix of every transient scope unit a holder is placed in.
pub const HOLDER_UNIT_PREFIX: &str = "qontinui-pty-holder-";

/// Every route spelling `--route` accepts, on every OS (an OS that cannot
/// take a route refuses it at spawn time, with a reason).
pub const ROUTES: [&str; 5] = ["auto", "plain", "scope", "breakaway", "wmi"];

/// Dispatch from `main()`. `Some(exit_code)` when argv[1] is exactly one of
/// the two spike flags, `None` otherwise (fall through to the GUI).
pub fn try_run_spike() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let first = args.first().and_then(|a| a.to_str())?;
    match first {
        SPIKE_FLAG => Some(run_holder(&args[1..])),
        SPIKE_PARENT_FLAG => Some(run_parent(&args[1..])),
        _ => None,
    }
}

/// Parsed `--pty-holder-spike` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderArgs {
    pub report_file: Option<PathBuf>,
    /// Program and arguments to run on the PTY. Never empty.
    pub command: Vec<OsString>,
}

/// Parsed `--pty-holder-spike-parent` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentArgs {
    /// One of [`ROUTES`].
    pub route: String,
    /// Where to write the relayed line, for a parent whose stdout nobody reads.
    pub report_file: Option<PathBuf>,
    /// Forwarded to the holder after `--`. Empty means the holder's default.
    pub command: Vec<OsString>,
}

/// The child a holder runs when none is given: long-lived and inert.
pub fn default_child_command() -> Vec<OsString> {
    #[cfg(windows)]
    let argv: &[&str] = &["cmd.exe", "/c", "timeout", "/t", "3600", "/nobreak"];
    #[cfg(not(windows))]
    let argv: &[&str] = &["sleep", "3600"];
    argv.iter().map(OsString::from).collect()
}

/// Split `args` at the first `--`: options before, command after.
fn split_command(args: &[OsString]) -> (&[OsString], Vec<OsString>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], args[i + 1..].to_vec()),
        None => (args, Vec::new()),
    }
}

pub fn parse_holder_args(args: &[OsString]) -> Result<HolderArgs, String> {
    let (opts, mut command) = split_command(args);
    let mut report_file = None;
    let mut it = opts.iter();
    while let Some(opt) = it.next() {
        match opt.to_str() {
            Some("--report-file") => {
                let path = it.next().ok_or("--report-file needs a path")?;
                report_file = Some(PathBuf::from(path));
            }
            _ => return Err(format!("unknown holder option {opt:?}")),
        }
    }
    if command.is_empty() {
        command = default_child_command();
    }
    Ok(HolderArgs {
        report_file,
        command,
    })
}

pub fn parse_parent_args(args: &[OsString]) -> Result<ParentArgs, String> {
    let (opts, command) = split_command(args);
    let mut route = "auto".to_string();
    let mut report_file = None;
    let mut it = opts.iter();
    while let Some(opt) = it.next() {
        match opt.to_str() {
            Some("--route") => {
                let r = it
                    .next()
                    .and_then(|r| r.to_str())
                    .ok_or_else(|| format!("--route needs one of {}", ROUTES.join("|")))?;
                if !ROUTES.contains(&r) {
                    return Err(format!("unknown route {r:?}"));
                }
                route = r.to_string();
            }
            Some("--report-file") => {
                let path = it.next().ok_or("--report-file needs a path")?;
                report_file = Some(PathBuf::from(path));
            }
            _ => return Err(format!("unknown parent option {opt:?}")),
        }
    }
    Ok(ParentArgs {
        route,
        report_file,
        command,
    })
}

/// The one machine-readable line a holder prints on success.
pub fn format_report(holder_pid: u32, child_pid: u32) -> String {
    format!("holder_pid={holder_pid} child_pid={child_pid}")
}

/// Read `key=<u32>` out of a report line.
pub fn report_field(line: &str, key: &str) -> Option<u32> {
    report_str(line, key).and_then(|v| v.parse().ok())
}

/// Read `key=<token>` out of a report line.
pub fn report_str<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|tok| tok.strip_prefix(key)?.strip_prefix('='))
}

/// Write `line` to `path` atomically: a temp name unique to this process,
/// then `rename`. Two writers can therefore never trample each other's temp.
pub fn write_report_atomic(path: &Path, line: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, format!("{line}\n"))?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Report `line`: report file first (its failure is returned, and also said on
/// stderr), then stdout. Stdout errors are ignored — the spawner may be gone.
fn emit_line(line: &str, report_file: Option<&Path>) -> Result<(), String> {
    let file_result = match report_file {
        Some(path) => write_report_atomic(path, line).map_err(|e| {
            let msg = format!("report-file {}: {e}", path.display());
            eprintln!("pty-holder-spike: {msg}");
            msg
        }),
        None => Ok(()),
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    file_result
}

/// Emit `<kind>_error=<msg>` and return `code`.
fn fail(kind: &str, msg: &str, report_file: Option<&Path>, code: i32) -> i32 {
    let _ = emit_line(&format!("{kind}_error={msg}"), report_file);
    code
}

/// Close every fd >= 3 this process inherited.
#[cfg(unix)]
fn close_inherited_fds() {
    let dir = if Path::new("/proc/self/fd").is_dir() {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    // Collect first: the directory handle is itself an fd, and is closed when
    // `read_dir` is dropped — closing its (now stale) number below is a
    // harmless EBADF, since nothing is opened in between.
    let fds: Vec<i32> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
            .collect(),
        Err(_) => return,
    };
    for fd in fds.into_iter().filter(|&fd| fd > 2) {
        // SAFETY: closing an fd number this process owns; nothing in this
        // process holds a Rust handle to any fd >= 3 at this point.
        unsafe {
            libc::close(fd);
        }
    }
}

#[cfg(unix)]
fn reset_sigchld() {
    // SAFETY: setting a default disposition has no memory-safety preconditions.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

/// Why the holder could not detach itself.
#[derive(Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
enum DetachError {
    /// `setsid()` refused because the holder leads its own process group —
    /// what a job-control shell (`bash` with monitor mode on) or
    /// `Command::process_group(0)` produces. Recoverable: see
    /// [`respawn_detached`].
    GroupLeader,
    Other(String),
}

#[cfg(unix)]
fn detach_from_spawner() -> Result<(), DetachError> {
    // SAFETY: setsid/getsid/getpid have no memory-safety preconditions.
    unsafe {
        if libc::setsid() != -1 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if libc::getsid(0) == libc::getpid() {
            // Already a session leader (e.g. spawned via setsid(1)): detached.
            return Ok(());
        }
        if err.raw_os_error() == Some(libc::EPERM) {
            return Err(DetachError::GroupLeader);
        }
        Err(DetachError::Other(format!("setsid failed: {err}")))
    }
}

/// A process-group leader cannot `setsid()`, and staying in that group would
/// leave the holder exposed to a kill of it. So re-exec once with `setsid()`
/// in `pre_exec` — the fresh child is never a group leader at that point —
/// relay its report line, and exit. The spawner's handle then names this
/// short-lived intermediate; the report line names the real holder. The
/// re-exec'd holder writes `--report-file` itself on success; this function
/// writes it only for its own failures.
#[cfg(unix)]
fn respawn_detached(args: &[OsString], report_file: Option<&Path>) -> i32 {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return fail("holder", &format!("current_exe: {e}"), report_file, 1),
    };
    let mut cmd = Command::new(exe);
    cmd.arg(SPIKE_FLAG)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: the closure only calls setsid(2), which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut holder = match cmd.spawn() {
        Ok(h) => h,
        Err(e) => return fail("holder", &format!("respawn: {e}"), report_file, 1),
    };
    let mut line = String::new();
    if let Some(out) = holder.stdout.take() {
        let _ = BufReader::new(out).read_line(&mut line);
    }
    let line = line.trim_end();
    if line.is_empty() {
        return fail("holder", "respawned holder printed nothing", report_file, 1);
    }
    let _ = emit_line(line, None);
    if report_field(line, "holder_pid").is_some() {
        0
    } else {
        1
    }
}

#[cfg(not(unix))]
fn detach_from_spawner() -> Result<(), DetachError> {
    // Windows: a process cannot leave a job it is already in, so detachment
    // is the spawner's creation flags (`holder_spawn`), not something the
    // holder can do for itself.
    Ok(())
}

/// Point fds 0/1/2 at `/dev/null` once the report line is out.
#[cfg(unix)]
fn silence_stdio() {
    // SAFETY: open/dup2/close on valid fds; failure leaves the fds as they were.
    unsafe {
        let fd = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if fd >= 0 {
            for target in 0..=2 {
                libc::dup2(fd, target);
            }
            if fd > 2 {
                libc::close(fd);
            }
        }
    }
}

#[cfg(not(unix))]
fn silence_stdio() {}

fn run_holder(args: &[OsString]) -> i32 {
    #[cfg(unix)]
    {
        close_inherited_fds();
        reset_sigchld();
    }
    let parsed = match parse_holder_args(args) {
        Ok(p) => p,
        Err(e) => return fail("holder", &e, None, 2),
    };
    let report_file = parsed.report_file.as_deref();
    match detach_from_spawner() {
        Ok(()) => {}
        #[cfg(unix)]
        Err(DetachError::GroupLeader) => return respawn_detached(args, report_file),
        #[cfg(not(unix))]
        Err(DetachError::GroupLeader) => unreachable!("only setsid() reports GroupLeader"),
        Err(DetachError::Other(e)) => return fail("holder", &e, report_file, 1),
    }

    use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, PtySize};
    let pair = match native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => return fail("holder", &format!("openpty: {e}"), report_file, 1),
    };
    let mut builder = CommandBuilder::new(&parsed.command[0]);
    builder.args(&parsed.command[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }
    let mut child = match pair.slave.spawn_command(builder) {
        Ok(c) => c,
        Err(e) => return fail("holder", &format!("spawn: {e}"), report_file, 1),
    };
    // The holder keeps only the master; the child holds the slave.
    drop(pair.slave);
    let master = pair.master;

    let Some(child_pid) = child.process_id() else {
        let _ = child.kill();
        return fail("holder", "child has no pid", report_file, 1);
    };
    if emit_line(&format_report(std::process::id(), child_pid), report_file).is_err() {
        // A spawner that asked for a report file cannot find this pane
        // without it: an unreported pane is a leak, so end it.
        let _ = child.kill();
        let _ = child.wait();
        return 1;
    }
    silence_stdio();

    // Serve nothing, but never let the child block on a full PTY buffer.
    if let Ok(mut reader) = master.try_clone_reader() {
        let _ = std::thread::Builder::new()
            .name("pty-holder-drain".into())
            .spawn(move || {
                let mut buf = [0u8; 8192];
                while matches!(reader.read(&mut buf), Ok(n) if n > 0) {}
            });
    }

    let code = match child.wait() {
        Ok(status) => status.exit_code() as i32,
        Err(_) => 1,
    };
    drop(master);
    code
}

/// A spawn route, resolved from the `--route` spelling for THIS OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedRoute {
    /// Direct spawn (Unix: the holder `setsid()`s itself).
    Plain,
    /// Linux: a transient systemd `--user` scope of the holder's own.
    Scope,
    /// Windows: [`qontinui_runner_win32::holder_spawn`] decides.
    WindowsAuto,
    /// Windows: an explicit `holder_spawn` route.
    Windows(&'static str),
}

impl ResolvedRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            ResolvedRoute::Plain => "plain",
            ResolvedRoute::Scope => "scope",
            ResolvedRoute::WindowsAuto => "auto",
            ResolvedRoute::Windows(r) => r,
        }
    }
}

/// True when a `/proc/<pid>/cgroup` body places the process inside a systemd
/// unit — some path component ends in `.service` or `.scope`. Such a unit's
/// stop (or crash-restart) kills everything in its cgroup under the default
/// `KillMode=control-group`, `setsid()` or not.
pub fn cgroup_is_in_systemd_unit(cgroup_file: &str) -> bool {
    cgroup_file.lines().any(|line| {
        line.splitn(3, ':').nth(2).is_some_and(|path| {
            path.split('/')
                .any(|c| c.ends_with(".service") || c.ends_with(".scope"))
        })
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
/// user bus) exists under `$XDG_RUNTIME_DIR`.
pub fn user_systemd_reachable() -> bool {
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return false;
    };
    let rt = PathBuf::from(rt);
    rt.join("systemd/private").exists() || rt.join("bus").exists()
}

/// Whether `auto` should take the `scope` route on this box, right now.
pub fn scope_route_applies() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/self/cgroup")
            .map(|c| cgroup_is_in_systemd_unit(&c))
            .unwrap_or(false)
        && find_systemd_run().is_some()
        && user_systemd_reachable()
}

/// Resolve a `--route` spelling for this OS. Refuses a route this OS cannot
/// take rather than silently substituting another.
pub fn resolve_route(route: &str) -> Result<ResolvedRoute, String> {
    if cfg!(windows) {
        return match route {
            "auto" => Ok(ResolvedRoute::WindowsAuto),
            "plain" => Ok(ResolvedRoute::Windows("plain")),
            "breakaway" => Ok(ResolvedRoute::Windows("breakaway")),
            "wmi" => Ok(ResolvedRoute::Windows("wmi")),
            _ => Err(format!("route {route:?} does not exist on Windows")),
        };
    }
    match route {
        "auto" if scope_route_applies() => Ok(ResolvedRoute::Scope),
        "auto" | "plain" => Ok(ResolvedRoute::Plain),
        "scope" if cfg!(target_os = "linux") => Ok(ResolvedRoute::Scope),
        _ => Err(format!("route {route:?} does not exist on this OS")),
    }
}

/// The holder's own argv (after the executable).
fn holder_args(command: &[OsString]) -> Vec<OsString> {
    let mut args = vec![OsString::from(SPIKE_FLAG)];
    if !command.is_empty() {
        args.push(OsString::from("--"));
        args.extend(command.iter().cloned());
    }
    args
}

/// `systemd-run --user --scope --quiet --collect --unit=<unit> -- <exe> <args>`.
/// In scope mode `systemd-run` registers ITSELF in the new scope and then
/// execs the command, so the holder (and the PTY child it forks) live in a
/// cgroup of their own, outside the spawner's unit.
pub fn scope_command(systemd_run: &Path, unit: &str, exe: &Path, args: &[OsString]) -> Command {
    let mut cmd = Command::new(systemd_run);
    cmd.args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg("--")
        .arg(exe)
        .args(args);
    cmd
}

/// A spawned holder plus, for the `scope` route, the unit it lives in.
struct SpawnedHolder {
    child: std::process::Child,
    unit: Option<String>,
}

fn spawn_holder_for_route(
    route: ResolvedRoute,
    command: &[OsString],
) -> Result<SpawnedHolder, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let args = holder_args(command);
    let (mut cmd, unit) = match route {
        ResolvedRoute::Scope => {
            let systemd_run = find_systemd_run().ok_or("scope route: systemd-run not on PATH")?;
            let unit = format!("{HOLDER_UNIT_PREFIX}{}", uuid::Uuid::new_v4().simple());
            (scope_command(&systemd_run, &unit, &exe, &args), Some(unit))
        }
        _ => {
            let mut cmd = Command::new(&exe);
            cmd.args(&args);
            (cmd, None)
        }
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = spawn_os(&mut cmd, route)?;
    Ok(SpawnedHolder { child, unit })
}

#[cfg(unix)]
fn spawn_os(cmd: &mut Command, _route: ResolvedRoute) -> Result<std::process::Child, String> {
    // The holder setsid()s itself; it must NOT be given its own process
    // group, or setsid() would refuse (it would recover, via a re-exec).
    cmd.spawn().map_err(|e| format!("spawn: {e}"))
}

#[cfg(windows)]
fn spawn_os(cmd: &mut Command, route: ResolvedRoute) -> Result<std::process::Child, String> {
    use qontinui_runner_win32::holder_spawn::{spawn_holder, spawn_holder_via, HolderSpawnRoute};
    match route {
        ResolvedRoute::WindowsAuto => spawn_holder(cmd)
            .map(|(child, _)| child)
            .map_err(|e| e.to_string()),
        ResolvedRoute::Windows(r) => {
            let r = HolderSpawnRoute::parse(r).ok_or_else(|| format!("unknown route {r:?}"))?;
            spawn_holder_via(cmd, r).map_err(|e| e.to_string())
        }
        other => Err(format!(
            "route {:?} does not exist on Windows",
            other.as_str()
        )),
    }
}

fn run_parent(args: &[OsString]) -> i32 {
    let parsed = match parse_parent_args(args) {
        Ok(p) => p,
        Err(e) => return fail("parent", &e, None, 2),
    };
    let report_file = parsed.report_file.as_deref();
    // Wait for the go line (EOF counts), so a test can place us in a job first.
    let mut go = String::new();
    let _ = std::io::stdin().lock().read_line(&mut go);

    let route = match resolve_route(&parsed.route) {
        Ok(r) => r,
        Err(e) => return fail("parent", &e, report_file, 2),
    };
    let mut holder = match spawn_holder_for_route(route, &parsed.command) {
        Ok(h) => h,
        Err(e) => return fail("parent", &e, report_file, 1),
    };
    let mut line = String::new();
    if let Some(out) = holder.child.stdout.take() {
        let _ = BufReader::new(out).read_line(&mut line);
    }
    let line = line.trim_end();
    if line.is_empty() {
        return fail("parent", "holder printed nothing", report_file, 1);
    }
    let mut relay = format!("{line} route={}", route.as_str());
    if let Some(unit) = &holder.unit {
        relay.push_str(&format!(" holder_unit={unit}"));
    }
    if emit_line(&relay, report_file).is_err() {
        return 1;
    }
    // Stay alive, holding the holder's Child handle, until killed.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Convenience for callers building a holder argv by hand.
pub fn holder_argv<'a>(command: impl IntoIterator<Item = &'a OsStr>) -> Vec<OsString> {
    let mut argv = vec![OsString::from(SPIKE_FLAG), OsString::from("--")];
    argv.extend(command.into_iter().map(OsStr::to_os_string));
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn pty_holder_spike_holder_args_default_to_a_long_lived_child() {
        let a = parse_holder_args(&[]).unwrap();
        assert_eq!(a.report_file, None);
        assert_eq!(a.command, default_child_command());
    }

    #[test]
    fn pty_holder_spike_holder_args_take_report_file_and_command() {
        let a = parse_holder_args(&os(&["--report-file", "/x/r", "--", "sleep", "5"])).unwrap();
        assert_eq!(a.report_file, Some(PathBuf::from("/x/r")));
        assert_eq!(a.command, os(&["sleep", "5"]));
    }

    #[test]
    fn pty_holder_spike_holder_args_refuse_unknown_options() {
        assert!(parse_holder_args(&os(&["--bogus"])).is_err());
        assert!(parse_holder_args(&os(&["--report-file"])).is_err());
    }

    #[test]
    fn pty_holder_spike_parent_args_parse_route_and_report_file() {
        let p = parse_parent_args(&os(&[
            "--route",
            "scope",
            "--report-file",
            "/r",
            "--",
            "sleep",
            "9",
        ]))
        .unwrap();
        assert_eq!(p.route, "scope");
        assert_eq!(p.report_file, Some(PathBuf::from("/r")));
        assert_eq!(p.command, os(&["sleep", "9"]));
        assert_eq!(parse_parent_args(&[]).unwrap().route, "auto");
        assert!(parse_parent_args(&os(&["--route", "group"])).is_err());
    }

    #[test]
    fn pty_holder_spike_report_line_round_trips() {
        let line = format_report(12, 345);
        assert_eq!(line, "holder_pid=12 child_pid=345");
        assert_eq!(report_field(&line, "holder_pid"), Some(12));
        assert_eq!(report_field(&line, "child_pid"), Some(345));
        let relay = format!("{line} route=scope holder_unit=qontinui-pty-holder-ab");
        assert_eq!(report_field(&relay, "child_pid"), Some(345));
        assert_eq!(report_str(&relay, "route"), Some("scope"));
        assert_eq!(
            report_str(&relay, "holder_unit"),
            Some("qontinui-pty-holder-ab")
        );
        assert_eq!(report_field("holder_error=x", "child_pid"), None);
    }

    #[test]
    fn pty_holder_spike_holder_argv_is_flag_dashdash_command() {
        let argv = holder_argv([OsStr::new("sleep"), OsStr::new("1")]);
        assert_eq!(argv, os(&[SPIKE_FLAG, "--", "sleep", "1"]));
        assert_eq!(holder_args(&os(&["sleep", "1"])), argv);
        assert_eq!(holder_args(&[]), os(&[SPIKE_FLAG]));
    }

    #[test]
    fn pty_holder_spike_cgroup_detection_reads_the_unit_component() {
        // This box's runner: a user service.
        assert!(cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/qontinui-runner.service\n"
        ));
        // A tmux/terminal scope.
        assert!(cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/tmux-spawn-x.scope\n"
        ));
        // cgroup v1 hybrid line with a unit in the name=systemd hierarchy.
        assert!(cgroup_is_in_systemd_unit(
            "12:cpu:/\n1:name=systemd:/system.slice/foo.service\n"
        ));
        // Root cgroup / container without systemd.
        assert!(!cgroup_is_in_systemd_unit("0::/\n"));
        assert!(!cgroup_is_in_systemd_unit("0::/docker/abc123\n"));
        assert!(!cgroup_is_in_systemd_unit(""));
    }

    #[test]
    fn pty_holder_spike_scope_command_puts_the_holder_in_its_own_unit() {
        let cmd = scope_command(
            Path::new("/usr/bin/systemd-run"),
            "qontinui-pty-holder-x",
            Path::new("/opt/runner"),
            &os(&[SPIKE_FLAG, "--", "sleep", "1"]),
        );
        assert_eq!(cmd.get_program(), OsStr::new("/usr/bin/systemd-run"));
        let args: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(
            args,
            [
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--unit=qontinui-pty-holder-x",
                "--",
                "/opt/runner",
                SPIKE_FLAG,
                "--",
                "sleep",
                "1"
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn pty_holder_spike_routes_resolve_per_os() {
        assert_eq!(
            resolve_route("plain").map(ResolvedRoute::as_str),
            Ok("plain")
        );
        assert!(resolve_route("nope").is_err());
        #[cfg(target_os = "linux")]
        {
            assert_eq!(resolve_route("scope"), Ok(ResolvedRoute::Scope));
            assert!(resolve_route("breakaway").is_err());
            assert!(resolve_route("wmi").is_err());
            let auto = resolve_route("auto").unwrap();
            assert_eq!(auto == ResolvedRoute::Scope, scope_route_applies());
        }
    }

    #[cfg(unix)]
    #[test]
    fn pty_holder_spike_report_file_is_atomic_and_failure_is_an_error() {
        let dir = std::env::temp_dir().join(format!(
            "pty-holder-spike-unit-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report");
        write_report_atomic(&path, "holder_pid=1 child_pid=2").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "holder_pid=1 child_pid=2\n"
        );
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        // A report file in a missing directory is an error, not a no-op.
        assert!(write_report_atomic(&dir.join("missing/report"), "x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
