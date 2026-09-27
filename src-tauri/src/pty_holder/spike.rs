//! Phase 0 survival spike: `--pty-holder-spike` and `--pty-holder-spike-parent`.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 0 — the hard go/no-go. Everything later in the plan assumes that a
//! PTY held by a process the runner spawned outlives the runner. This module
//! is the smallest thing that can be killed to find out.
//!
//! ## `--pty-holder-spike [--report-file <path>] [-- <program> <args>…]`
//!
//! 1. Detaches from its spawner. Unix: `setsid()`, so the holder is in neither
//!    the spawner's session nor its process group — a hangup of the spawner's
//!    terminal, or a kill of its process group, does not reach it. A holder
//!    that already LEADS a process group (job-control shell, `process_group(0)`)
//!    cannot `setsid()`; it re-execs itself once with `setsid()` in `pre_exec`
//!    and relays the real holder's line — measured on merytshost, where a
//!    plain `cmd &` from the agent's bash hit exactly this. Windows:
//!    detachment is decided by the SPAWNER's creation flags
//!    ([`qontinui_runner_win32::holder_spawn`]), because a process cannot leave
//!    a job it is already in.
//! 2. Opens a PTY through `portable-pty` and spawns the child on it (default: a
//!    long `sleep` / `timeout`).
//! 3. Prints exactly ONE line to stdout — `holder_pid=<n> child_pid=<n>`, or
//!    `holder_error=<reason>` — and, when asked, writes the same line to
//!    `--report-file` (atomically). The file exists for the WMI route, whose
//!    holder has no inherited stdout to print to.
//! 4. Unix: points stdin/stdout/stderr at `/dev/null`, so a dead spawner's pipe
//!    can neither deliver `SIGPIPE` nor keep the spawner's reader open.
//! 5. Keeps the master open and serves nothing: one thread drains the master so
//!    the child never blocks on a full PTY buffer, and the main thread waits on
//!    the child. The holder exits with the child's exit code.
//!
//! It never ignores `SIGHUP`: dispositions set to `SIG_IGN` survive `exec`, so
//! doing so would make the CHILD immune to its own terminal's hangup — the
//! signal that must still end a pane whose holder dies.
//!
//! ## `--pty-holder-spike-parent [--route <auto|plain|breakaway|wmi>] [-- <program> <args>…]`
//!
//! The spawner stand-in the integration tests kill. It waits for one line on
//! stdin (so a test can put it in a job object before it spawns anything),
//! spawns a holder the way the runner will — through
//! [`qontinui_runner_win32::holder_spawn`] on Windows — relays the holder's
//! line with ` route=<route>` appended, and then sleeps until it is killed.
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
    /// `auto`, `plain`, `breakaway` or `wmi`. Only `auto` is meaningful off
    /// Windows.
    pub route: String,
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
    let mut it = opts.iter();
    while let Some(opt) = it.next() {
        match opt.to_str() {
            Some("--route") => {
                let r = it
                    .next()
                    .and_then(|r| r.to_str())
                    .ok_or("--route needs one of auto|plain|breakaway|wmi")?;
                if !matches!(r, "auto" | "plain" | "breakaway" | "wmi") {
                    return Err(format!("unknown route {r:?}"));
                }
                route = r.to_string();
            }
            _ => return Err(format!("unknown parent option {opt:?}")),
        }
    }
    Ok(ParentArgs { route, command })
}

/// The one machine-readable line a holder prints on success.
pub fn format_report(holder_pid: u32, child_pid: u32) -> String {
    format!("holder_pid={holder_pid} child_pid={child_pid}")
}

/// Read `key=<u32>` out of a report line.
pub fn report_field(line: &str, key: &str) -> Option<u32> {
    line.split_whitespace()
        .find_map(|tok| tok.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
}

fn emit_line(line: &str, report_file: Option<&Path>) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    if let Some(path) = report_file {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, format!("{line}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
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
/// short-lived intermediate; the report line names the real holder.
#[cfg(unix)]
fn respawn_detached(args: &[OsString]) -> i32 {
    use std::os::unix::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            emit_line(&format!("holder_error=current_exe: {e}"), None);
            return 1;
        }
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
        Err(e) => {
            emit_line(&format!("holder_error=respawn: {e}"), None);
            return 1;
        }
    };
    let mut line = String::new();
    if let Some(out) = holder.stdout.take() {
        let _ = BufReader::new(out).read_line(&mut line);
    }
    let line = line.trim_end();
    // The respawned holder wrote --report-file itself; only relay stdout.
    emit_line(line, None);
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
    let parsed = match parse_holder_args(args) {
        Ok(p) => p,
        Err(e) => {
            emit_line(&format!("holder_error={e}"), None);
            return 2;
        }
    };
    let report_file = parsed.report_file.as_deref();
    match detach_from_spawner() {
        Ok(()) => {}
        #[cfg(unix)]
        Err(DetachError::GroupLeader) => return respawn_detached(args),
        #[cfg(not(unix))]
        Err(DetachError::GroupLeader) => unreachable!("only setsid() reports GroupLeader"),
        Err(DetachError::Other(e)) => {
            emit_line(&format!("holder_error={e}"), report_file);
            return 1;
        }
    }

    use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, PtySize};
    let pair = match native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            emit_line(&format!("holder_error=openpty: {e}"), report_file);
            return 1;
        }
    };
    let mut builder = CommandBuilder::new(&parsed.command[0]);
    builder.args(&parsed.command[1..]);
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }
    let mut child = match pair.slave.spawn_command(builder) {
        Ok(c) => c,
        Err(e) => {
            emit_line(&format!("holder_error=spawn: {e}"), report_file);
            return 1;
        }
    };
    // The holder keeps only the master; the child holds the slave.
    drop(pair.slave);
    let master = pair.master;

    let Some(child_pid) = child.process_id() else {
        let _ = child.kill();
        emit_line("holder_error=child has no pid", report_file);
        return 1;
    };
    emit_line(&format_report(std::process::id(), child_pid), report_file);
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

fn holder_command(command: &[OsString]) -> std::io::Result<Command> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg(SPIKE_FLAG);
    if !command.is_empty() {
        cmd.arg("--");
        cmd.args(command.iter().map(OsString::as_os_str));
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(cmd)
}

#[cfg(unix)]
fn spawn_holder_for_route(cmd: &mut Command, route: &str) -> Result<std::process::Child, String> {
    if route != "auto" {
        return Err(format!("route {route:?} exists only on Windows"));
    }
    // The holder setsid()s itself; the spawner needs no special flags. It must
    // NOT be given its own process group, or setsid() would refuse.
    cmd.spawn().map_err(|e| format!("spawn: {e}"))
}

#[cfg(windows)]
fn spawn_holder_for_route(cmd: &mut Command, route: &str) -> Result<std::process::Child, String> {
    use qontinui_runner_win32::holder_spawn::{spawn_holder, spawn_holder_via, HolderSpawnRoute};
    if route == "auto" {
        return spawn_holder(cmd)
            .map(|(child, _)| child)
            .map_err(|e| e.to_string());
    }
    let r = HolderSpawnRoute::parse(route).ok_or_else(|| format!("unknown route {route:?}"))?;
    spawn_holder_via(cmd, r).map_err(|e| e.to_string())
}

fn run_parent(args: &[OsString]) -> i32 {
    let parsed = match parse_parent_args(args) {
        Ok(p) => p,
        Err(e) => {
            println!("parent_error={e}");
            return 2;
        }
    };
    // Wait for the go line, so a test can place us in a job first.
    let mut go = String::new();
    let _ = std::io::stdin().lock().read_line(&mut go);

    let mut cmd = match holder_command(&parsed.command) {
        Ok(c) => c,
        Err(e) => {
            println!("parent_error=current_exe: {e}");
            return 1;
        }
    };
    let mut holder = match spawn_holder_for_route(&mut cmd, &parsed.route) {
        Ok(h) => h,
        Err(e) => {
            println!("parent_error={e}");
            let _ = std::io::stdout().flush();
            return 1;
        }
    };
    let mut line = String::new();
    if let Some(out) = holder.stdout.take() {
        let _ = BufReader::new(out).read_line(&mut line);
    }
    let line = line.trim_end();
    if line.is_empty() {
        println!("parent_error=holder printed nothing");
    } else {
        println!("{line} route={}", parsed.route);
    }
    let _ = std::io::stdout().flush();
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
    fn pty_holder_spike_parent_args_parse_route() {
        let p = parse_parent_args(&os(&["--route", "breakaway", "--", "sleep", "9"])).unwrap();
        assert_eq!(p.route, "breakaway");
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
        assert_eq!(
            report_field(&format!("{line} route=auto"), "child_pid"),
            Some(345)
        );
        assert_eq!(report_field("holder_error=x", "child_pid"), None);
    }

    #[test]
    fn pty_holder_spike_holder_argv_is_flag_dashdash_command() {
        let argv = holder_argv([OsStr::new("sleep"), OsStr::new("1")]);
        assert_eq!(argv, os(&[SPIKE_FLAG, "--", "sleep", "1"]));
    }
}
