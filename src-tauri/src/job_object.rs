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
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
