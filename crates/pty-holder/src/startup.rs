//! Holder start-up hygiene: what a process that must outlive its spawner does
//! first.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`.
//! Moved here from the Phase 0 spike (deleted later in Phase 2) when Phase 2 put the PTY into the dedicated holder
//! binary — the Phase 0 hand-off lists each of them as a requirement of the
//! real holder:
//!
//! 1. [`close_inherited_fds`] — close every fd >= 3 the spawner leaked (a
//!    socket, a pipe end, a lock), so nothing of the runner's is pinned for the
//!    pane's lifetime.
//! 2. [`reset_sigchld`] — `SIGCHLD` back to `SIG_DFL`, so waiting on the child
//!    works whatever disposition the spawner left.
//! 3. [`detach_from_spawner`] — Unix `setsid()`: the holder leaves the
//!    spawner's session AND process group, so a hangup of the spawner's
//!    terminal or a kill of its process group does not reach it. It does NOT
//!    leave the spawner's cgroup — that is the SPAWNER's job (`crate::spawn`,
//!    the `systemd-run --user --scope` route). Windows: detachment is decided
//!    by the spawner's creation flags (`qontinui_runner_win32::holder_spawn`),
//!    because a process cannot leave a job it is already in.
//! 4. [`silence_stdio`] — once the ready line is out, fds 0/1/2 point at
//!    `/dev/null`, so the holder stops holding the spawner's pipes: the
//!    spawner's reader sees EOF instead of being pinned open for the pane's
//!    lifetime.
//!
//! **A holder that leads its own process group refuses to start** rather than
//! re-exec'ing itself (which the spike did): a re-exec makes the spawner's
//! process handle name a short-lived intermediate instead of the holder, and
//! the spawner's verified-pid teardown requires the reported holder pid to BE
//! the process it spawned. `crate::spawn` never makes a holder a group leader
//! (no `process_group(0)`), so this only refuses a hand launch from a
//! job-control shell — loudly, with a typed `holder_error=detach`.
//!
//! The holder never ignores `SIGHUP`: a `SIG_IGN` disposition survives `exec`,
//! which would make the CHILD immune to its own terminal's hangup — the signal
//! that must still end a pane whose holder dies.

/// Close every fd >= 3 this process inherited.
#[cfg(unix)]
pub fn close_inherited_fds() {
    let dir = if std::path::Path::new("/proc/self/fd").is_dir() {
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
        // process holds a Rust handle to any fd >= 3 at this point (callers run
        // this first thing in `main`).
        unsafe {
            libc::close(fd);
        }
    }
}

/// `SIGCHLD` back to its default disposition.
#[cfg(unix)]
pub fn reset_sigchld() {
    // SAFETY: setting a default disposition has no memory-safety preconditions.
    unsafe {
        libc::signal(libc::SIGCHLD, libc::SIG_DFL);
    }
}

/// Why the holder could not detach itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachError {
    /// `setsid()` refused because this process leads its own process group —
    /// what a job-control shell or `Command::process_group(0)` produces.
    GroupLeader,
    Other(String),
}

impl std::fmt::Display for DetachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetachError::GroupLeader => f.write_str(
                "group_leader: this process leads its process group, so setsid() is refused; \
                 spawn the holder outside its own process group",
            ),
            DetachError::Other(m) => f.write_str(m),
        }
    }
}

/// Unix: `setsid()`. Already a session leader (e.g. launched via `setsid(1)`)
/// counts as detached.
#[cfg(unix)]
pub fn detach_from_spawner() -> Result<(), DetachError> {
    // SAFETY: setsid/getsid/getpid have no memory-safety preconditions.
    unsafe {
        if libc::setsid() != -1 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if libc::getsid(0) == libc::getpid() {
            return Ok(());
        }
        if err.raw_os_error() == Some(libc::EPERM) {
            return Err(DetachError::GroupLeader);
        }
        Err(DetachError::Other(format!("setsid failed: {err}")))
    }
}

/// Windows: nothing a process can do for itself; see the module docs.
#[cfg(not(unix))]
pub fn detach_from_spawner() -> Result<(), DetachError> {
    Ok(())
}

/// Point fds 0/1/2 at `/dev/null`.
#[cfg(unix)]
pub fn silence_stdio() {
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

/// Windows: a `DETACHED_PROCESS` holder has no console; its stdio handles are
/// the spawner's pipes, which close when the spawner reads EOF or exits.
#[cfg(not(unix))]
pub fn silence_stdio() {}
