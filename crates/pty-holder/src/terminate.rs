//! Out-of-band termination of a pane's holder, for a CLIENT that can no longer
//! drive it through the protocol (its `kill` went unanswered, or it held its
//! lock without answering a handshake for the whole reconnect window).
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 2 review round 2 (N7), and the Phase 0 hand-off rule "teardown signals
//! only verified pids". A pid is a NAME, and a name can be reused the moment
//! its process is reaped. So the order is:
//!
//! 1. **Take a handle on the process first** — Linux `pidfd_open`, Windows
//!    `OpenProcess`. From here on the handle refers to ONE process, whatever
//!    later happens to the pid.
//! 2. **Then verify** that this pane's lock is HELD right now and its record
//!    names `holder_pid`. A holder that had died before step 1 left its lock
//!    acquirable (the lock is an `flock` on a close-on-exec descriptor, so no
//!    child inherits it), so a held lock naming the pid after step 1 means the
//!    process behind the handle is that live holder.
//! 3. **Signal through the handle** — `pidfd_send_signal(SIGKILL)` /
//!    `TerminateProcess` — never `kill(pid)` / `taskkill /PID`.
//!
//! The holder's death ends its child: on Unix its PTY master closes and the
//! child gets `SIGHUP`; on Windows the holder-owned `KILL_ON_JOB_CLOSE` job
//! holding the child's tree closes with the holder's last handle.
//!
//! Residual: on Unix systems without `pidfd_open` (macOS; Linux < 5.3) step 1
//! is impossible and the signal is a plain `kill(pid)` after step 2 — a pid
//! reuse in the instants between verification and signal is not excluded
//! there, and the detail string says which path ran.

use std::path::Path;

use crate::lock::{read_record, PaneLock, TryLock};
use crate::pane::{lock_path, PaneId};
use crate::spawn::signalable_pid;

/// What [`terminate_verified_holder`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Termination {
    /// A kill was actually delivered to the verified holder.
    pub signalled: bool,
    /// Human-readable: which path ran, or why nothing was signalled.
    pub detail: String,
}

impl Termination {
    fn refused(detail: impl Into<String>) -> Self {
        Termination {
            signalled: false,
            detail: format!("not signalled: {}", detail.into()),
        }
    }
}

/// Step 2 of the module docs: the pane's lock is held and names `holder_pid`.
fn verify_holder(pane_dir: &Path, pane_id: &PaneId, holder_pid: u32) -> Result<(), String> {
    let lock_file = lock_path(pane_dir, pane_id);
    match PaneLock::try_acquire_existing(&lock_file) {
        Ok(TryLock::Held) => {}
        Ok(TryLock::Acquired(lock)) => {
            drop(lock);
            return Err("the pane's lock is free — the holder is already dead".into());
        }
        Err(e) => return Err(format!("lock unreadable: {e}")),
    }
    match read_record(&lock_file) {
        Some(r) if r.holder_pid == holder_pid => Ok(()),
        Some(r) => Err(format!(
            "the lock names holder pid {}, not {holder_pid}",
            r.holder_pid
        )),
        None => Err("no lock record to verify the pid against".into()),
    }
}

/// End `holder_pid`, the holder this client attached to for `pane_id`, only
/// once it is verified to still be that holder. See the module docs.
pub fn terminate_verified_holder(
    pane_dir: &Path,
    pane_id: &PaneId,
    holder_pid: u32,
) -> Termination {
    let Some(pid) = signalable_pid(holder_pid) else {
        return Termination::refused(format!("pid {holder_pid} out of range"));
    };
    imp::terminate(pid, || verify_holder(pane_dir, pane_id, holder_pid))
}

#[cfg(target_os = "linux")]
mod imp {
    use super::Termination;

    pub(super) fn terminate(pid: i32, verify: impl FnOnce() -> Result<(), String>) -> Termination {
        // SAFETY: pidfd_open(pid, 0) takes no pointers; the result is either
        // -1 or a new descriptor owned (and closed) below.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOSYS) {
                return super::plain_kill(pid, verify, "pidfd_open unavailable");
            }
            return Termination::refused(format!("pidfd_open({pid}): {err}"));
        }
        let fd = fd as libc::c_int;
        let result = match verify() {
            Err(why) => Termination::refused(why),
            Ok(()) => {
                // SAFETY: a pidfd we own; null siginfo, no flags.
                let rc = unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        fd,
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                };
                if rc == 0 {
                    Termination {
                        signalled: true,
                        detail: "SIGKILL to the verified holder through its pidfd".into(),
                    }
                } else {
                    Termination::refused(format!(
                        "pidfd_send_signal: {}",
                        std::io::Error::last_os_error()
                    ))
                }
            }
        };
        // SAFETY: closing the descriptor pidfd_open returned, exactly once.
        unsafe {
            libc::close(fd);
        }
        result
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
mod imp {
    use super::Termination;

    pub(super) fn terminate(pid: i32, verify: impl FnOnce() -> Result<(), String>) -> Termination {
        super::plain_kill(pid, verify, "no pidfd on this platform")
    }
}

/// Unix without a process handle: verify, then `kill(pid)`. See the module
/// docs' residual.
#[cfg(unix)]
fn plain_kill(pid: i32, verify: impl FnOnce() -> Result<(), String>, why: &str) -> Termination {
    if let Err(e) = verify() {
        return Termination::refused(e);
    }
    // SAFETY: a plain signal to a pid in 2..=i32::MAX whose pane lock was just
    // verified held and naming it.
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        Termination {
            signalled: true,
            detail: format!("SIGKILL to the verified holder by pid ({why})"),
        }
    } else {
        Termination::refused(format!("kill: {}", std::io::Error::last_os_error()))
    }
}

#[cfg(windows)]
mod imp {
    use super::Termination;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    pub(super) fn terminate(pid: i32, verify: impl FnOnce() -> Result<(), String>) -> Termination {
        // SAFETY: OpenProcess takes no pointers; a non-null result is a handle
        // we own and close below.
        let h = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid as u32) };
        if h.is_null() {
            return Termination::refused(format!(
                "OpenProcess({pid}): {}",
                std::io::Error::last_os_error()
            ));
        }
        let result = match verify() {
            Err(why) => Termination::refused(why),
            Ok(()) => {
                // SAFETY: a valid process handle opened with PROCESS_TERMINATE.
                if unsafe { TerminateProcess(h, 1) } != 0 {
                    Termination {
                        signalled: true,
                        detail: "TerminateProcess on the verified holder's handle (its job \
                                 ends the child tree)"
                            .into(),
                    }
                } else {
                    Termination::refused(format!(
                        "TerminateProcess: {}",
                        std::io::Error::last_os_error()
                    ))
                }
            }
        };
        // SAFETY: closing the handle OpenProcess returned, exactly once.
        unsafe { CloseHandle(h) };
        result
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::lock::LockRecord;
    use std::time::{Duration, Instant};

    fn record(pid: u32) -> LockRecord {
        LockRecord {
            holder_pid: pid,
            child_pid: None,
            versions: vec![2],
            started_at_unix_ms: 0,
            holder_build: "test".into(),
        }
    }

    /// A free lock (dead holder; its pid may be recycled) and a held lock
    /// naming another pid are refused; a held lock naming the pid is killed —
    /// through its pidfd.
    #[test]
    fn pty_holder_terminate_signals_only_a_verified_holder() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("ptyt-{}-{nanos:x}", std::process::id()));
        crate::pane::prepare_private_dir(&dir).unwrap();
        let id = PaneId::new("t").unwrap();
        let lock_file = lock_path(&dir, &id);
        // A stand-in "holder" this test owns.
        let mut victim = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();

        {
            let TryLock::Acquired(mut lock) = PaneLock::try_acquire(&lock_file).unwrap() else {
                panic!("fresh lock");
            };
            lock.write_record(&record(victim.id())).unwrap();
        }
        let t = terminate_verified_holder(&dir, &id, victim.id());
        assert!(!t.signalled, "{t:?}");
        assert!(
            victim.try_wait().unwrap().is_none(),
            "a free lock's pid was signalled"
        );

        let TryLock::Acquired(mut lock) = PaneLock::try_acquire(&lock_file).unwrap() else {
            panic!("lock");
        };
        lock.write_record(&record(victim.id() + 1)).unwrap();
        let t = terminate_verified_holder(&dir, &id, victim.id());
        assert!(!t.signalled, "{t:?}");
        assert!(victim.try_wait().unwrap().is_none());

        lock.write_record(&record(victim.id())).unwrap();
        let t = terminate_verified_holder(&dir, &id, victim.id());
        assert!(t.signalled, "{t:?}");
        assert!(t.detail.contains("pidfd"), "{t:?}");
        let end = Instant::now() + Duration::from_secs(5);
        while victim.try_wait().unwrap().is_none() {
            assert!(Instant::now() < end, "the verified holder was not killed");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !terminate_verified_holder(&dir, &id, 1).signalled,
            "pid 1 is never signalled"
        );
        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
