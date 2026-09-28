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
//!   systemd unit's cgroup ([`cgroup_is_in_systemd_unit`]) AND `systemd-run`
//!   and a user systemd socket are present, else `plain`. Socket presence is
//!   not proof the scope can be created, so an `auto` scope spawn that fails
//!   (systemd-run exits non-zero, or prints nothing) FALLS BACK to `plain` and
//!   says so: `route=plain fallback_from=scope reason=<systemd-run stderr>`.
//!   Whenever the holder ends up `plain` inside a detected unit cgroup, the
//!   relay carries `unprotected=cgroup`: the pane WILL die with that unit, and
//!   the spawner must surface it rather than claim survival. Windows:
//!   [`qontinui_runner_win32::holder_spawn::spawn_holder`] (`IsProcessInJob`).
//! - `plain` (Unix + Windows) — a direct spawn; the holder `setsid()`s itself.
//! - `scope` (Linux) — `systemd-run --user --scope --collect
//!   --unit=qontinui-pty-holder-<uuid> -- <exe> --pty-holder-spike …`: the
//!   holder gets its OWN transient scope, so stopping (or crash-restarting) the
//!   spawner's `KillMode=control-group` service does not reach it. A FORCED
//!   `--route scope` never falls back: it fails loudly with systemd-run's
//!   stderr.
//!
//!   **What `scope` does NOT protect against.** The scope is a unit of the
//!   same USER MANAGER (`user@<uid>.service`) as the runner. Restarting the
//!   user manager, or logging out without linger enabled
//!   (`loginctl enable-linger`), stops every unit under it — every holder
//!   and every pane included. Phase 0's GO covers the runner's OWN unit being
//!   stopped, restarted or crash-restarted; it does not cover the user
//!   manager going away.
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

/// How long a spawner waits for a holder's one report line, in ms. A holder
/// that is alive but silent (e.g. `systemd-run` blocked on a wedged user
/// manager) is treated as having printed nothing once this passes, so the
/// failure, teardown and `auto` fallback paths all still run. Spike-only
/// override; default [`DEFAULT_REPORT_TIMEOUT`].
pub const REPORT_TIMEOUT_ENV: &str = "QONTINUI_PTY_HOLDER_SPIKE_REPORT_TIMEOUT_MS";
/// Default for [`REPORT_TIMEOUT_ENV`]: 120 s, the same allowance the
/// integration tests give a slow debug holder on a loaded box — a deadline
/// shorter than that would kill a slow-but-healthy holder as "silent".
pub const DEFAULT_REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long [`SpawnedHolder::abandon`] lets `systemctl --user stop --no-block`
/// take before killing it: `--no-block` only enqueues the stop job, but the
/// D-Bus call itself can still hang on a wedged user manager.
pub const SYSTEMCTL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// TEST HOOK (spike-only): a comma list of [`HostFacts`] fields to force
/// `true` — `in_unit_cgroup`, `scope_tooling` — so the `auto`-scope paths can
/// be exercised on a box (or CI runner) that is not inside a systemd unit.
/// It only ever widens what `auto` TRIES; the spawn itself is still real.
pub const FORCE_FACTS_ENV: &str = "QONTINUI_PTY_HOLDER_SPIKE_FORCE_FACTS";

/// The report deadline in effect for this process.
pub fn report_timeout() -> std::time::Duration {
    std::env::var(REPORT_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(DEFAULT_REPORT_TIMEOUT)
}

/// Read one line on a helper thread, giving up after `timeout`. `None` means
/// the deadline passed (the helper thread stays parked on the pipe until its
/// writer goes away, which the caller's teardown then causes).
fn read_line_within<R: Read + Send + 'static>(
    reader: R,
    timeout: std::time::Duration,
) -> Option<String> {
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
fn read_all_within<R: Read + Send + 'static>(
    mut reader: R,
    timeout: std::time::Duration,
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

/// `(holder_pid, child_pid)` from a SUCCESS report line only — never from an
/// error line, which can embed arbitrary text (a report-file path, a relayed
/// line) — and only when both are real, signalable process ids: `2..=i32::MAX`.
/// Anything else is `None`, so no caller can turn a parse into `kill(0)`,
/// `kill(-1)` or a signal to init.
pub fn success_pids(line: &str) -> Option<(i32, i32)> {
    if !line.starts_with("holder_pid=") || is_error_line(line) {
        return None;
    }
    let pid = |v: u32| (v > 1 && v <= i32::MAX as u32).then_some(v as i32);
    Some((
        pid(report_field(line, "holder_pid")?)?,
        pid(report_field(line, "child_pid")?)?,
    ))
}

/// The PTY child named by a success `line`, but only if `/proc` confirms it is
/// still THAT process: its parent is the reported holder and it leads its own
/// session (portable-pty's child `setsid()`s onto the PTY). `None` when the
/// identity cannot be verified — callers then rely on the holder's death (or
/// the scope unit's stop) instead of signalling a pid that may be recycled.
///
/// **Only confirms on Linux** (it reads `/proc`). Elsewhere it always answers
/// `None`, so off Linux a teardown never signals the PTY child explicitly.
#[cfg(target_os = "linux")]
pub fn verified_pty_child(line: &str) -> Option<i32> {
    let (holder, child) = success_pids(line)?;
    let raw = std::fs::read_to_string(format!("/proc/{child}/stat")).ok()?;
    // The comm field is parenthesised and may itself contain ')' or spaces, so
    // the fields we want start after the LAST ')'.
    let (_, rest) = raw.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ppid: i32 = f.get(1)?.parse().ok()?;
    let session: i32 = f.get(3)?.parse().ok()?;
    (ppid == holder && session == child).then_some(child)
}

/// Non-Linux: identity cannot be confirmed, so never a pid (see the Linux doc).
#[cfg(not(target_os = "linux"))]
pub fn verified_pty_child(_line: &str) -> Option<i32> {
    None
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

/// True for a `<kind>_error=` line.
pub fn is_error_line(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|t| {
        t.split_once('=')
            .is_some_and(|(k, _)| k.ends_with("_error"))
    })
}

/// Report `line` as `kind` (`holder` / `parent`): report file first, then
/// stdout. If the report file cannot be written, stdout carries
/// `<kind>_error=report-file …` INSTEAD of `line` — a spawner must never read
/// success for a report that did not land — and the error is returned (and
/// said on stderr). An error `line` is printed as-is either way. Stdout write
/// errors are ignored: the spawner may be gone.
fn emit_line(kind: &str, line: &str, report_file: Option<&Path>) -> Result<(), String> {
    let file_result = match report_file {
        Some(path) => write_report_atomic(path, line).map_err(|e| {
            let msg = format!("report-file {}: {e}", path.display());
            eprintln!("pty-holder-spike: {msg}");
            msg
        }),
        None => Ok(()),
    };
    let printed = match &file_result {
        Err(msg) if !is_error_line(line) => format!("{kind}_error={msg}"),
        _ => line.to_string(),
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{printed}");
    let _ = out.flush();
    file_result
}

/// Emit `<kind>_error=<msg>` and return `code`.
fn fail(kind: &str, msg: &str, report_file: Option<&Path>, code: i32) -> i32 {
    let _ = emit_line(kind, &format!("{kind}_error={msg}"), report_file);
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
    // console-ok: unix-only re-exec (respawn_detached is #[cfg(unix)]).
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
    let timeout = report_timeout();
    let line = holder
        .stdout
        .take()
        .and_then(|out| read_line_within(out, timeout))
        .unwrap_or_default();
    let line = line.as_str();
    if line.is_empty() {
        let _ = holder.kill();
        let _ = holder.wait();
        return fail(
            "holder",
            &format!(
                "respawned holder printed nothing within {}ms",
                timeout.as_millis()
            ),
            report_file,
            1,
        );
    }
    let _ = emit_line("holder", line, None);
    // An error line (e.g. the real holder's report file failed) is relayed
    // verbatim and is a failure: exit non-zero so the spawner cannot mistake
    // this intermediate's clean exit for a live pane.
    if !is_error_line(line) && report_field(line, "holder_pid").is_some() {
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
    if emit_line(
        "holder",
        &format_report(std::process::id(), child_pid),
        report_file,
    )
    .is_err()
    {
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
/// unit whose stop the runner itself can trigger — its own service (system or
/// `app.slice` user service) or a launcher's scope (`tmux-spawn-*.scope`,
/// `app-*.scope`). Such a unit's stop or crash-restart kills everything in
/// its cgroup under the default `KillMode=control-group`, `setsid()` or not.
///
/// Only the DEEPEST unit component counts — that is the unit the process is
/// actually in; the ones above it are ancestors. Excluded when deepest:
/// - `user@<uid>.service` and its `init.scope`: the user manager itself. A
///   scope route cannot escape it (the scope lives under it too — see the
///   module docs), so claiming protection there would be false.
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
/// user bus) exists under `$XDG_RUNTIME_DIR`.
pub fn user_systemd_reachable() -> bool {
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return false;
    };
    let rt = PathBuf::from(rt);
    rt.join("systemd/private").exists() || rt.join("bus").exists()
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
    pub fn probe() -> Self {
        let linux = cfg!(target_os = "linux");
        let forced = std::env::var(FORCE_FACTS_ENV).unwrap_or_default();
        let force = |name: &str| linux && forced.split(',').any(|f| f.trim() == name);
        HostFacts {
            windows: cfg!(windows),
            linux,
            in_unit_cgroup: force("in_unit_cgroup")
                || (linux
                    && std::fs::read_to_string("/proc/self/cgroup")
                        .map(|c| cgroup_is_in_systemd_unit(&c))
                        .unwrap_or(false)),
            scope_tooling: force("scope_tooling")
                || (linux && find_systemd_run().is_some() && user_systemd_reachable()),
        }
    }
}

/// A resolved route plus what the spawner must be told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDecision {
    pub route: ResolvedRoute,
    /// The holder will share a systemd unit's cgroup and die with that unit.
    pub unprotected_cgroup: bool,
    /// `auto` chose this route, so a failed scope spawn may fall back.
    pub may_fall_back: bool,
}

/// Decide a `--route` spelling against `facts`. Refuses a route this OS
/// cannot take rather than silently substituting another.
pub fn decide_route(route: &str, facts: &HostFacts) -> Result<RouteDecision, String> {
    let decision = |route, may_fall_back| RouteDecision {
        route,
        unprotected_cgroup: route == ResolvedRoute::Plain && facts.in_unit_cgroup,
        may_fall_back,
    };
    if facts.windows {
        return match route {
            "auto" => Ok(decision(ResolvedRoute::WindowsAuto, false)),
            "plain" | "breakaway" | "wmi" => Ok(decision(
                ResolvedRoute::Windows(match route {
                    "plain" => "plain",
                    "breakaway" => "breakaway",
                    _ => "wmi",
                }),
                false,
            )),
            _ => Err(format!("route {route:?} does not exist on Windows")),
        };
    }
    match route {
        "auto" if facts.in_unit_cgroup && facts.scope_tooling => {
            Ok(decision(ResolvedRoute::Scope, true))
        }
        "auto" | "plain" => Ok(decision(ResolvedRoute::Plain, false)),
        "scope" if facts.linux => Ok(decision(ResolvedRoute::Scope, false)),
        _ => Err(format!("route {route:?} does not exist on this OS")),
    }
}

/// [`decide_route`] against this box, now.
pub fn resolve_route(route: &str) -> Result<RouteDecision, String> {
    decide_route(route, &HostFacts::probe())
}

/// Whether `auto` would try the `scope` route on this box, right now.
pub fn scope_route_applies() -> bool {
    resolve_route("auto").is_ok_and(|d| d.route == ResolvedRoute::Scope)
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
    // console-ok: systemd-run exists only on Linux; the scope route is refused on Windows.
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
            // console-ok: on Windows spawn_os hands this builder to runner-win32's
            // spawn_holder, which sets DETACHED_PROCESS (no console) itself.
            let mut cmd = Command::new(&exe);
            cmd.args(&args);
            (cmd, None)
        }
    };
    // systemd-run's stderr is the only account of WHY a scope failed; the
    // holder itself points fd 2 at /dev/null once it reports, so the pipe
    // does not stay pinned.
    let stderr = if unit.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr);
    let child = spawn_os(&mut cmd, route)?;
    Ok(SpawnedHolder { child, unit })
}

impl SpawnedHolder {
    /// Read the holder's one report line within [`report_timeout`]. `Ok("")`
    /// is EOF with nothing printed; `Err` names the missed deadline. Either
    /// way the caller treats it as "printed nothing".
    fn read_report(&mut self) -> Result<String, String> {
        let timeout = report_timeout();
        let Some(out) = self.child.stdout.take() else {
            return Ok(String::new());
        };
        read_line_within(out, timeout)
            .ok_or_else(|| format!("no report within {}ms", timeout.as_millis()))
    }

    /// Why a holder that printed nothing failed, as one whitespace-free
    /// token: its exit status and whatever systemd-run said on stderr. Waits
    /// up to 3 s for the process to exit, KILLS it if it has not, then reads
    /// stderr — itself bounded, since a grandchild could still hold the pipe.
    fn failure_reason(&mut self) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(s)) => break s.to_string(),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break "still-running(killed)".to_string();
                }
            }
        };
        let err = self
            .child
            .stderr
            .take()
            .map(|e| {
                read_all_within(e, std::time::Duration::from_secs(2))
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

    /// Whether `line` is a success line whose `holder_pid` is the process we
    /// actually spawned. On the plain and scope routes the holder IS that
    /// process (`systemd-run --scope` execs it); anything else is not a
    /// report this spawner can trust.
    fn reports_this_holder(&self, line: &str) -> bool {
        success_pids(line).is_some_and(|(holder, _)| holder as u32 == self.child.id())
    }

    /// Leave nothing behind, in this order:
    /// 1. SIGKILL the PTY child named by `line` — only a SUCCESS line whose
    ///    `holder_pid` is our own spawned process, and only once
    ///    [`verified_pty_child`] confirms it is still that process (Linux
    ///    only); unverifiable → no explicit signal (steps 2-3 still end it:
    ///    the unit stop kills its cgroup, and the holder's death hangs up its
    ///    PTY).
    /// 2. Stop the holder's scope unit, if any, with `--no-block` behind
    ///    [`SYSTEMCTL_DEADLINE`] — the unit's processes are killed by the
    ///    manager asynchronously.
    /// 3. Kill and reap the holder process itself (the scope route's holder
    ///    is our direct child, so this does not wait on the manager).
    ///
    /// End-to-end bound: at most [`SYSTEMCTL_DEADLINE`] (5 s) plus a
    /// SIGKILL-and-reap, even with a wedged user manager.
    fn abandon(&mut self, line: &str) {
        #[cfg(unix)]
        if let Some(child_pid) = verified_pty_child(line).filter(|_| self.reports_this_holder(line))
        {
            // SAFETY: a plain signal to a pid in 2..=i32::MAX whose parent and
            // session were just verified against our own holder's report.
            unsafe {
                libc::kill(child_pid, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = line;
        if let Some(unit) = &self.unit {
            // console-ok: only reached with a scope unit, i.e. on Linux.
            let spawned = Command::new("systemctl")
                .args(["--user", "stop", "--no-block", "--quiet", unit])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut systemctl) = spawned {
                let deadline = std::time::Instant::now() + SYSTEMCTL_DEADLINE;
                while matches!(systemctl.try_wait(), Ok(None))
                    && std::time::Instant::now() < deadline
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                let _ = systemctl.kill();
                let _ = systemctl.wait();
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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

    let decision = match resolve_route(&parsed.route) {
        Ok(d) => d,
        Err(e) => return fail("parent", &e, report_file, 2),
    };
    let mut route = decision.route;
    let mut unprotected = decision.unprotected_cgroup;
    let mut fallback: Option<String> = None;

    let (mut holder, line) = loop {
        let attempt = spawn_holder_for_route(route, &parsed.command).and_then(|mut h| {
            let what = match h.read_report() {
                Ok(line) if !line.is_empty() => return Ok((h, line)),
                Ok(_) => "holder printed nothing".to_string(),
                Err(deadline) => format!("holder printed nothing: {deadline}"),
            };
            let reason = h.failure_reason();
            h.abandon("");
            Err(format!("{what} (route={}) reason={reason}", route.as_str()))
        });
        match attempt {
            Ok(ok) => break ok,
            // M2: only an `auto`-chosen scope falls back; a forced one fails.
            Err(e) if route == ResolvedRoute::Scope && decision.may_fall_back => {
                let reason = e.split_whitespace().collect::<Vec<_>>().join("_");
                fallback = Some(reason.chars().take(300).collect());
                route = ResolvedRoute::Plain;
                unprotected = true; // still inside the unit cgroup we detected
            }
            Err(e) => return fail("parent", &e, report_file, 1),
        }
    };

    if is_error_line(&line) {
        holder.abandon(&line);
        return fail("parent", &format!("holder failed: {line}"), report_file, 1);
    }
    if !holder.reports_this_holder(&line) {
        // Not a report from the process we spawned: trust none of its pids.
        let spawned = holder.child.id();
        holder.abandon("");
        return fail(
            "parent",
            &format!("holder report does not name the spawned holder (pid {spawned}): {line}"),
            report_file,
            1,
        );
    }
    let mut relay = format!("{line} route={}", route.as_str());
    if let Some(reason) = &fallback {
        relay.push_str(&format!(" fallback_from=scope reason={reason}"));
    }
    if unprotected {
        relay.push_str(" unprotected=cgroup");
    }
    if let Some(unit) = &holder.unit {
        relay.push_str(&format!(" holder_unit={unit}"));
    }
    if let Err(e) = emit_line("parent", &relay, report_file) {
        // emit_line already put `parent_error=` on stdout; say which unit it
        // was so a caller can verify it is gone, then leave nothing behind.
        holder.abandon(&line);
        if let Some(unit) = &holder.unit {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "parent_error=abandoned holder_unit={unit} after: {e}");
            let _ = out.flush();
        }
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
        // The user manager itself, its init.scope, and a login session are
        // NOT units whose stop the runner triggers.
        assert!(!cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service\n"
        ));
        assert!(!cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/init.scope\n"
        ));
        assert!(!cgroup_is_in_systemd_unit(
            "0::/user.slice/user-1000.slice/session-3.scope\n"
        ));
        // A system service is.
        assert!(cgroup_is_in_systemd_unit(
            "0::/system.slice/qontinui-runner.service\n"
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

    const LINUX_IN_UNIT: HostFacts = HostFacts {
        windows: false,
        linux: true,
        in_unit_cgroup: true,
        scope_tooling: true,
    };

    #[test]
    fn pty_holder_spike_routes_resolve_per_os() {
        let f = LINUX_IN_UNIT;
        assert_eq!(
            decide_route("plain", &f).map(|d| d.route.as_str()),
            Ok("plain")
        );
        assert!(decide_route("nope", &f).is_err());
        assert!(decide_route("breakaway", &f).is_err());
        assert!(decide_route("wmi", &f).is_err());
        let forced = decide_route("scope", &f).unwrap();
        assert_eq!(forced.route, ResolvedRoute::Scope);
        assert!(!forced.may_fall_back, "a forced scope must never fall back");
        let auto = decide_route("auto", &f).unwrap();
        assert_eq!(auto.route, ResolvedRoute::Scope);
        assert!(auto.may_fall_back);
        assert!(!auto.unprotected_cgroup);

        let win = HostFacts {
            windows: true,
            linux: false,
            in_unit_cgroup: false,
            scope_tooling: false,
        };
        assert_eq!(
            decide_route("auto", &win).unwrap().route,
            ResolvedRoute::WindowsAuto
        );
        assert!(decide_route("scope", &win).is_err());
    }

    /// M3: a detected unit cgroup with no usable user systemd must not be a
    /// silent `plain` — it is `plain` AND `unprotected`.
    #[test]
    fn pty_holder_spike_auto_in_unit_without_systemd_is_unprotected() {
        let no_systemd = HostFacts {
            scope_tooling: false,
            ..LINUX_IN_UNIT
        };
        let d = decide_route("auto", &no_systemd).unwrap();
        assert_eq!(d.route, ResolvedRoute::Plain);
        assert!(d.unprotected_cgroup);
        // Forcing plain inside a unit is equally unprotected, and says so.
        assert!(
            decide_route("plain", &LINUX_IN_UNIT)
                .unwrap()
                .unprotected_cgroup
        );
        // Outside any unit, plain is simply plain.
        let outside = HostFacts {
            in_unit_cgroup: false,
            ..LINUX_IN_UNIT
        };
        let d = decide_route("auto", &outside).unwrap();
        assert_eq!(d.route, ResolvedRoute::Plain);
        assert!(!d.unprotected_cgroup);
    }

    #[test]
    fn pty_holder_spike_success_pids_parse_only_real_pids_from_success_lines() {
        assert_eq!(
            success_pids("holder_pid=100 child_pid=200 route=plain"),
            Some((100, 200))
        );
        // An error line never yields pids, whatever it embeds.
        assert_eq!(
            success_pids("holder_error=report-file /tmp/holder_pid=5 child_pid=0: denied"),
            None
        );
        assert_eq!(
            success_pids("parent_error=holder failed: holder_pid=7 child_pid=8"),
            None
        );
        // 0, 1 and anything that would not survive `as i32` are rejected.
        assert_eq!(success_pids("holder_pid=100 child_pid=0"), None);
        assert_eq!(success_pids("holder_pid=100 child_pid=1"), None);
        assert_eq!(success_pids("holder_pid=100 child_pid=4294967295"), None);
        assert_eq!(success_pids("holder_pid=100 child_pid=2147483648"), None);
        assert_eq!(success_pids("holder_pid=100 child_pid=-1"), None);
        assert_eq!(success_pids("holder_pid=1 child_pid=200"), None);
        assert_eq!(
            success_pids("holder_pid=100 child_pid=2147483647"),
            Some((100, i32::MAX))
        );
        assert_eq!(success_pids(""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pty_holder_spike_verified_pty_child_refuses_an_unrelated_process() {
        // This test process is not a session-leading child of pid 2.
        let me = std::process::id();
        assert_eq!(
            verified_pty_child(&format!("holder_pid=2 child_pid={me}")),
            None
        );
        assert_eq!(verified_pty_child("holder_error=x child_pid=0"), None);
    }

    #[test]
    fn pty_holder_spike_error_lines_are_recognised() {
        assert!(is_error_line("holder_error=report-file /x: denied"));
        assert!(is_error_line("parent_error=holder printed nothing"));
        assert!(!is_error_line("holder_pid=1 child_pid=2"));
        assert!(!is_error_line(""));
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
