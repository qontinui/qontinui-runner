//! Process starts the upgrade needs: a detached process in its own session
//! (the self-managed guard, a rolled-back runner), and a successor that
//! inherits the listener fds under the `LISTEN_FDS` protocol (plan D1,
//! "Self-managed"). The fd passing follows `pty-holder/src/handoff.rs`
//! `spawn_successor`: high-numbered close-on-exec copies, `dup2`'d onto
//! 3, 4, … in `pre_exec`, close-on-exec cleared on the targets.

use std::path::Path;
use std::time::Duration;

/// Start `exe args…` with null stdio in a new session, so it outlives this
/// process and is in nobody's process group. Its pid.
#[cfg(unix)]
pub fn spawn_detached(exe: &Path, args: &[String]) -> Result<u32, String> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches only the child.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("start {}: {e}", exe.display()))?;
    Ok(child.id())
}

#[cfg(not(unix))]
pub fn spawn_detached(exe: &Path, _args: &[String]) -> Result<u32, String> {
    Err(format!(
        "a detached {} is the Windows self-managed path (plan Phase 8)",
        exe.display()
    ))
}

/// Start the successor `exe args…` with `fds` at 3, 4, … and `LISTEN_FDS`,
/// `LISTEN_FDNAMES` and `LISTEN_PID` (its own pid — set by a one-line `sh`
/// trampoline, `LISTEN_PID=$$ exec …`, because the pid is known only after
/// the fork and an environment write is not async-signal-safe there).
#[cfg(unix)]
pub fn spawn_successor(exe: &Path, args: &[String], fds: &[(String, i32)]) -> Result<u32, String> {
    use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    if fds.is_empty() {
        return Err("no listener fd to pass".to_string());
    }
    // High-numbered CLOEXEC copies, so a dup2 onto 3.. can never clobber a
    // source that happens to sit on a target number.
    const ABOVE: i32 = 64;
    let mut copies: Vec<OwnedFd> = Vec::with_capacity(fds.len());
    for (name, fd) in fds {
        // SAFETY: the fd is a listener this process holds open for its whole
        // life (registered by the server that serves it); borrowing it only
        // to duplicate it.
        let borrowed = unsafe { BorrowedFd::borrow_raw(*fd) };
        let dup = rustix_dup_cloexec(borrowed.as_raw_fd(), ABOVE)
            .map_err(|e| format!("duplicate listener {name} (fd {fd}): {e}"))?;
        copies.push(dup);
    }
    let sources: Vec<i32> = copies.iter().map(AsRawFd::as_raw_fd).collect();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("LISTEN_PID=$$; export LISTEN_PID; exec \"$0\" \"$@\"")
        .arg(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in super::listener_fds::listen_env(fds) {
        cmd.env(k, v);
    }
    // SAFETY: only dup2, fcntl and setsid run between fork and exec — all
    // async-signal-safe — on fds open in the child.
    unsafe {
        cmd.pre_exec(move || {
            for (i, src) in sources.iter().enumerate() {
                let dst = 3 + i as i32;
                if libc::dup2(*src, dst) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(dst, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn();
    drop(copies);
    let child = child.map_err(|e| format!("start the successor {}: {e}", exe.display()))?;
    Ok(child.id())
}

#[cfg(unix)]
fn rustix_dup_cloexec(fd: i32, above: i32) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    // SAFETY: F_DUPFD_CLOEXEC duplicates an open fd to the lowest free number
    // >= `above`; the result is owned by nothing else.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, above) };
    if new < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `new` was just created above and is owned here only.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(new) })
}

#[cfg(not(unix))]
pub fn spawn_successor(
    exe: &Path,
    _args: &[String],
    _fds: &[(String, i32)],
) -> Result<u32, String> {
    Err(format!(
        "starting {} with inherited listeners is the Windows handoff (plan Phase 8)",
        exe.display()
    ))
}

/// End `pid`: SIGTERM, then SIGKILL once `grace` has passed. When `pid` is
/// this process's own child (an aborted upgrade's successor or guard) it is
/// reaped as it exits: an unreaped child stays a zombie that reads as alive,
/// and the wait would run its whole grace for nothing.
#[cfg(unix)]
pub fn terminate(pid: u32, grace: Duration) -> Result<(), String> {
    let p = i32::try_from(pid).map_err(|_| format!("pid {pid} out of range"))?;
    // SAFETY: sending a signal to a pid.
    if unsafe { libc::kill(p, libc::SIGTERM) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(format!("SIGTERM {pid}: {e}"));
    }
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if reaped(p) || !super::sys::pid_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // SAFETY: as above.
    unsafe { libc::kill(p, libc::SIGKILL) };
    // SIGKILL is not instant: confirm the death (and reap our own child)
    // before the caller acts as if the pid were gone.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if reaped(p) || !super::sys::pid_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{pid} is still alive 2 s after SIGKILL"))
}

/// Reap `pid` if it is this process's child and has exited. `false` for a
/// running child and for any pid that is not our child (`ECHILD`).
#[cfg(unix)]
fn reaped(pid: i32) -> bool {
    let mut status = 0;
    // SAFETY: a non-blocking wait on one pid; `status` outlives the call.
    unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) == pid }
}

#[cfg(not(unix))]
pub fn terminate(pid: u32, _grace: Duration) -> Result<(), String> {
    Err(format!("ending {pid} is the Windows guard (plan Phase 8)"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The successor sees the listener at fd 3, `LISTEN_FDS=1`,
    /// `LISTEN_FDNAMES=api` and `LISTEN_PID` equal to its OWN pid — the
    /// check sd_listen_fds(3) makes before adopting.
    #[test]
    fn successor_inherits_the_listener_under_the_listen_fds_protocol() {
        use std::os::fd::AsRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen");
        let script = dir.path().join("successor.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ -e /proc/self/fd/3 ] && fd3=yes || fd3=no\n\
                 echo \"$LISTEN_FDS $LISTEN_FDNAMES $fd3 $([ \"$LISTEN_PID\" = \"$$\" ] && echo pid-ok || echo pid-bad) $1\" > {}\n",
                out.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let pid = spawn_successor(
            &script,
            &["--upgrade-successor".to_string()],
            &[("api".to_string(), listener.as_raw_fd())],
        )
        .unwrap();
        assert!(pid > 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !out.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let seen = std::fs::read_to_string(&out).unwrap();
        assert_eq!(seen.trim(), "1 api yes pid-ok --upgrade-successor");
        // The parent still owns its listener.
        assert_eq!(listener.local_addr().unwrap().port(), port);
    }

    #[test]
    fn terminate_ends_a_process_and_tolerates_a_gone_one() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        // Our own child: reaped as it exits, so the call returns well inside
        // its grace instead of waiting it out on a zombie.
        let started = std::time::Instant::now();
        terminate(pid, Duration::from_secs(10)).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "terminate waited {:?} on its own exited child",
            started.elapsed()
        );
        // The pid is reaped and free now; signalling it again could reach an
        // unrelated process that took the number. A gone pid is tolerated:
        // one above pid_max can never exist (ESRCH).
        drop(child);
        let pid_max: u32 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(4_194_304);
        assert!(terminate(pid_max + 1, Duration::from_millis(10)).is_ok());
    }

    /// A process that ignores SIGTERM outlives the grace, takes SIGKILL, and
    /// `terminate` confirms that death before returning `Ok`. Linux only: the
    /// wait for the `exec` reads `/proc`.
    #[cfg(target_os = "linux")]
    #[test]
    fn terminate_escalates_to_sigkill_and_confirms_the_death() {
        // `trap "" TERM` sets SIGTERM to ignored, which `exec` keeps.
        let child = std::process::Command::new("sh")
            .args(["-c", "trap '' TERM; exec sleep 30"])
            .spawn()
            .unwrap();
        let pid = child.id();
        // Let the shell install the trap and exec before the signal lands.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|c| c.trim() != "sleep")
            .unwrap_or(false)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{pid} never exec'd sleep"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let started = std::time::Instant::now();
        terminate(pid, Duration::from_millis(500)).unwrap();
        let took = started.elapsed();
        assert!(
            took >= Duration::from_millis(500),
            "SIGTERM was not ignored ({took:?})"
        );
        assert!(took < Duration::from_secs(5), "terminate took {took:?}");
        // `terminate` reaped its own child, so the pid is free now and is
        // deliberately not probed again (it could name a new process).
        drop(child);
    }
}
