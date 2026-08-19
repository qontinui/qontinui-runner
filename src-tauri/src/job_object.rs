//! Windows Job Object for automatic child process cleanup.
//!
//! When the runner process exits (even via crash or taskkill), Windows will
//! automatically terminate all processes assigned to this Job Object. This
//! prevents orphaned Claude CLI processes from accumulating.

use std::sync::OnceLock;
use tracing::{error, info, warn};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

// CreateJobObjectW requires the Win32_Security feature (for SECURITY_ATTRIBUTES param type).
// Declare it directly via extern to avoid the conditional feature gate, since we only
// pass null for both parameters anyway.
extern "system" {
    fn CreateJobObjectW(lpjobattributes: *const std::ffi::c_void, lpname: *const u16) -> HANDLE;
}

/// RAII wrapper for a Windows Job Object handle.
struct JobObjectHandle(HANDLE);

// SAFETY: The Job Object handle is used only via AssignProcessToJobObject,
// which is thread-safe. The handle itself is never mutated after creation.
unsafe impl Send for JobObjectHandle {}
unsafe impl Sync for JobObjectHandle {}

impl Drop for JobObjectHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Global singleton Job Object. Initialized once at startup.
static JOB_OBJECT: OnceLock<JobObjectHandle> = OnceLock::new();

/// Initialize the global Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
///
/// Must be called once at application startup (before spawning any child processes).
/// If initialization fails, child processes will not be auto-killed on crash,
/// but the explicit taskkill in the shutdown handler still provides cleanup.
pub fn init_job_object() {
    JOB_OBJECT.get_or_init(|| {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                error!(
                    "Failed to create Job Object (error {}). Child processes will not be auto-killed on crash.",
                    std::io::Error::last_os_error()
                );
                // Return a sentinel so we don't retry
                return JobObjectHandle(std::ptr::null_mut());
            }

            // Configure: kill all assigned processes when the Job Object handle is closed
            // (which happens automatically when the runner process exits).
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let result = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );

            if result == 0 {
                error!(
                    "Failed to configure Job Object (error {}). Closing handle.",
                    std::io::Error::last_os_error()
                );
                CloseHandle(handle);
                return JobObjectHandle(std::ptr::null_mut());
            }

            info!("Windows Job Object initialized (kill-on-close enabled)");
            JobObjectHandle(handle)
        }
    });
}

/// Assign a child process to the global Job Object.
///
/// When the runner exits (even via crash), Windows will automatically kill
/// all assigned processes. This is a safety net — the explicit taskkill
/// in the shutdown handler provides the primary cleanup.
///
/// # Safety
/// The `process_handle` must be a valid Windows process HANDLE.
pub fn assign_process_to_job(process_handle: HANDLE) {
    if let Some(job) = JOB_OBJECT.get() {
        if job.0.is_null() || job.0 == INVALID_HANDLE_VALUE {
            // Job Object failed to initialize — skip silently
            return;
        }
        unsafe {
            let result = AssignProcessToJobObject(job.0, process_handle);
            if result == 0 {
                warn!(
                    "Failed to assign process to Job Object (error {})",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

/// A dedicated, OWNED Job Object whose close reaps every process assigned
/// to it. The handle is RAII — dropping it closes the job, and
/// kill-on-close terminates the assigned process AND everything it
/// spawned. A process may belong to both this job and the global one
/// (nested jobs, Win8+).
///
/// Two callers, both needing tree-scoped cleanup the global singleton
/// cannot give (it only fires when the whole runner exits):
///
/// - CI builds pass a memory limit — the CI-build OOM backstop
///   (runner-as-CI-node plan §4.6: "job-object memory limit as a
///   backstop", load-bearing not polish). A memory limit on the global
///   job would throttle every runner child (agents, terminals), so CI
///   builds get their own job.
/// - The agent pusher passes `None` — it needs only the kill-on-close
///   tree reap, to stop a timed-out `git push` leaking its transport
///   helper (see `agent_pusher::push_one`).
pub struct ScopedKillOnCloseJob(HANDLE);

// SAFETY: same contract as JobObjectHandle — the handle is only used via
// thread-safe Job Object APIs and never mutated after creation.
unsafe impl Send for ScopedKillOnCloseJob {}
unsafe impl Sync for ScopedKillOnCloseJob {}

impl Drop for ScopedKillOnCloseJob {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

impl ScopedKillOnCloseJob {
    /// Create a job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, optionally
    /// adding a `JOB_OBJECT_LIMIT_JOB_MEMORY` commit ceiling of
    /// `memory_limit_bytes` across all assigned processes. Pass `None` for
    /// the kill-on-close reap alone. `None` on any API failure — callers
    /// degrade to the global job only (a missing backstop must not block
    /// the caller).
    pub fn create(memory_limit_bytes: Option<usize>) -> Option<Self> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                warn!(
                    "ScopedKillOnCloseJob: CreateJobObjectW failed (error {})",
                    std::io::Error::last_os_error()
                );
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = match memory_limit_bytes {
                Some(limit) => {
                    info.JobMemoryLimit = limit;
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY
                }
                None => JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            };
            let result = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if result == 0 {
                warn!(
                    "ScopedKillOnCloseJob: SetInformationJobObject failed (error {})",
                    std::io::Error::last_os_error()
                );
                CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }

    /// Assign a child process to this job. Failure is a logged warning —
    /// the global job's kill-on-close still applies to the child.
    ///
    /// # Safety contract
    /// `process_handle` must be a valid Windows process HANDLE (the same
    /// contract as [`assign_process_to_job`]).
    pub fn assign(&self, process_handle: HANDLE) {
        unsafe {
            let result = AssignProcessToJobObject(self.0, process_handle);
            if result == 0 {
                warn!(
                    "ScopedKillOnCloseJob: assign failed (error {})",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// `None` must still yield a usable job — the agent pusher's case,
    /// where only the kill-on-close reap is wanted and a memory ceiling
    /// would be wrong (a push is not a build).
    #[test]
    fn create_without_a_memory_limit_yields_a_job() {
        let job = ScopedKillOnCloseJob::create(None);
        assert!(
            job.is_some(),
            "kill-on-close job creation should succeed on Windows"
        );
    }

    /// Closing the job must terminate what was assigned to it. This is the
    /// property `agent_pusher::push_one` leans on: `kill_on_drop` reaps
    /// only the direct child, so a timed-out `git push` used to leave its
    /// `git-remote-https` transport helper running. Descendants inherit
    /// job membership, so reaping the assigned process reaps the tree.
    #[test]
    fn dropping_the_job_kills_the_assigned_process() {
        let job = ScopedKillOnCloseJob::create(None).expect("create job");

        // A child that would otherwise outlive this test by ~60s.
        let mut child = Command::new("cmd.exe")
            .args(["/c", "ping", "-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        job.assign(child.as_raw_handle() as _);
        drop(job);

        // Kill-on-close is asynchronous; poll rather than assume.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_) => break,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    panic!("child survived the job close — kill-on-close did not fire");
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}
