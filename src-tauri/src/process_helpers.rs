//! Helper functions for spawning child processes on Windows without console windows.
//!
//! On Windows, spawning any console program (cmd, powershell, python, node, npx,
//! git, cargo, etc.) from a GUI app creates a visible console window. These helpers
//! set `CREATE_NO_WINDOW` to suppress that. On non-Windows platforms they are
//! transparent passthroughs.

/// Create a `std::process::Command` with `CREATE_NO_WINDOW` on Windows.
pub fn no_window<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Create a `tokio::process::Command` with `CREATE_NO_WINDOW` on Windows.
pub fn tokio_no_window<S: AsRef<std::ffi::OsStr>>(program: S) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        #[allow(unused_imports)]
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Convenience alias: `no_window("cmd.exe")`.
pub fn cmd_no_window() -> std::process::Command {
    no_window("cmd.exe")
}

/// Convenience alias: `tokio_no_window("cmd.exe")`.
pub fn tokio_cmd_no_window() -> tokio::process::Command {
    tokio_no_window("cmd.exe")
}

/// Reap a child's whole PROCESS TREE when this guard drops — the thing
/// [`std::process::Child::kill`] does NOT do.
///
/// `Child::kill` is `TerminateProcess` on ONE process. Windows has no `exec`, so
/// a *shim* — a rustup proxy, a volta/pyenv-win shim, any `cmd /c` line — runs
/// the real tool as a GRANDCHILD that inherited the pipe write ends. Killing the
/// child alone leaves that grandchild alive AND holding duplicates of those
/// handles, so anything waiting for EOF on the pipes waits forever and the
/// process itself is simply orphaned.
///
/// Attaching the child to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
/// makes dropping this guard terminate every process in the tree, which both
/// reaps the orphan and closes the inherited handles.
///
/// **Best-effort by construction.** Creating the job or assigning the child can
/// fail, and no caller may fail because its reaper did: a failed attach degrades
/// to exactly the previous behaviour (the child alone is killed). Distinct from
/// `job_object`'s global singleton, which is a runner-lifetime net owned by the
/// binary crate; this one is a scoped, per-child guard usable from the lib.
///
/// Non-Windows: a no-op placeholder, because the grandchild shape is far rarer
/// where shims `exec` in place. A process group would be the equivalent fix.
#[cfg(windows)]
pub struct ChildTreeGuard(Option<windows_sys::Win32::Foundation::HANDLE>);

// SAFETY: the handle is only ever passed to `AssignProcessToJobObject` /
// `CloseHandle`, both thread-safe, and is never mutated after creation.
#[cfg(windows)]
unsafe impl Send for ChildTreeGuard {}
#[cfg(windows)]
unsafe impl Sync for ChildTreeGuard {}

#[cfg(windows)]
impl ChildTreeGuard {
    /// Put `child` (and everything it goes on to spawn) in a kill-on-close job.
    ///
    /// Racy at the edges on purpose: std offers no pre-spawn hook, so a
    /// grandchild created in the microseconds before the assignment lands is not
    /// covered. Every realistic shim spawns its target well after that, and the
    /// callers that care do not depend on the guard for correctness.
    pub fn attach(child: &std::process::Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() || job == INVALID_HANDLE_VALUE {
                return Self(None);
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .unwrap_or(0),
            );
            if set == 0 {
                CloseHandle(job);
                return Self(None);
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as _) == 0 {
                // Closing an EMPTY kill-on-close job kills nothing, so this is a
                // clean degrade rather than a half-armed guard.
                CloseHandle(job);
                return Self(None);
            }
            Self(Some(job))
        }
    }
}

#[cfg(windows)]
impl Drop for ChildTreeGuard {
    fn drop(&mut self) {
        if let Some(job) = self.0.take() {
            // Kill-on-close: this closes the last handle to the job, which
            // terminates every process still in it.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
        }
    }
}

/// Non-Windows placeholder — see the Windows form for what this is for.
#[cfg(not(windows))]
pub struct ChildTreeGuard;

#[cfg(not(windows))]
impl ChildTreeGuard {
    /// No-op: nothing is attached and nothing is reaped.
    pub fn attach(_child: &std::process::Child) -> Self {
        Self
    }
}
