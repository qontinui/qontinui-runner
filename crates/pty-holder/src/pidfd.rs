//! A Linux process HANDLE: `pidfd_open(2)` and `pidfd_send_signal(2)`.
//!
//! Plan `2026-10-25-runner-kill-paths-never-kill-anything-off-windows`, Phase 1
//! (vet D5). A pid is a name, recycled once its process is reaped; a pidfd
//! names the PROCESS. A signal sent through one either reaches the process the
//! descriptor was opened on or fails with `ESRCH` because that process has been
//! reaped — it never reaches a stranger that later got the same pid. So every
//! kill in this crate takes a pidfd FIRST, checks identity second, and
//! signals through the descriptor third ([`crate::reap`], [`crate::terminate`],
//! [`crate::tree_freeze`]). This module is the one implementation of the
//! descriptor itself; it used to exist twice, privately, in `reap` and
//! `terminate`.
//!
//! `pidfd_open` exists from Linux 5.3. On an older kernel [`open`] answers
//! [`Opened::NoPidFd`], and each caller falls back to its identity re-check
//! followed by a plain `kill(pid)`, and says so.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// An open pidfd, closed on drop.
#[derive(Debug)]
pub struct PidFd(OwnedFd);

/// What [`open`] found.
#[derive(Debug)]
pub enum Opened {
    /// A handle on the process `pid` named at the instant of the call.
    Fd(PidFd),
    /// `pidfd_open` does not exist on this kernel (`ENOSYS`).
    NoPidFd,
    /// No such process (`ESRCH`), or it is not openable; the error says which.
    Gone(io::Error),
}

/// Take a handle on the process `pid` names right now. The caller passes a
/// pid it has already range-checked (`crate::spawn::signalable_pid`).
pub fn open(pid: i32) -> Opened {
    // SAFETY: pidfd_open(pid, 0) takes no pointers; the result is -1 or a new
    // descriptor that nothing else owns.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd >= 0 {
        // SAFETY: a descriptor pidfd_open just returned, owned only by this
        // value from here on and closed exactly once, on drop.
        return Opened::Fd(PidFd(unsafe { OwnedFd::from_raw_fd(fd as RawFd) }));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ENOSYS) {
        Opened::NoPidFd
    } else {
        Opened::Gone(err)
    }
}

impl PidFd {
    /// Send `sig` to the process behind this descriptor. `sig == 0` sends
    /// nothing and answers whether that process still exists (alive or a
    /// zombie): `Err(ESRCH)` once it has been reaped.
    pub fn send_signal(&self, sig: libc::c_int) -> io::Result<()> {
        // SAFETY: a pidfd this value owns; a null siginfo (the kernel then
        // fills an SI_USER one, as kill(2) does) and no flags.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                sig,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Whether the process behind this descriptor has exited (a zombie, or
    /// reaped): a pidfd polls readable from that moment. A failed poll answers
    /// `false` — an unreadable state is never reported as "exited".
    pub fn has_exited(&self) -> bool {
        let mut p = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call; a zero
        // timeout, so it never blocks.
        let rc = unsafe { libc::poll(&mut p, 1, 0) };
        rc == 1 && p.revents & libc::POLLIN != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The handle outlives the pid's meaning: once the process is reaped, a
    /// signal through it fails instead of reaching whatever reuses the pid.
    #[test]
    fn pty_holder_pidfd_tracks_one_process_through_exit_and_reap() {
        let mut child = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .unwrap();
        let fd = match open(child.id() as i32) {
            Opened::Fd(fd) => fd,
            other => {
                // Never leave the fixture running for 300 s.
                let _ = child.kill();
                let _ = child.wait();
                panic!("pidfd_open of a live child failed: {other:?}");
            }
        };
        assert!(fd.send_signal(0).is_ok());
        assert!(!fd.has_exited());
        fd.send_signal(libc::SIGKILL).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !fd.has_exited() {
            assert!(std::time::Instant::now() < end, "SIGKILL never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // A zombie still exists for a signal; a reaped process does not.
        assert!(fd.send_signal(0).is_ok());
        child.wait().unwrap();
        assert_eq!(
            fd.send_signal(0).unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
