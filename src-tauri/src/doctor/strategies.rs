//! OS-level health check strategies for the Doctor service.
//!
//! Implements process health inspection using Windows APIs:
//! - CPU time sampling via GetProcessTimes
//! - Process tree enumeration via CreateToolhelp32Snapshot
//! - Memory monitoring via GetProcessMemoryInfo

/// Result of a health check for a single process.
#[derive(Debug)]
pub struct HealthCheckResult {
    /// Whether the process is still alive.
    pub process_alive: bool,
    /// Total CPU time in 100-nanosecond intervals (kernel + user).
    /// None if the process couldn't be queried.
    pub cpu_time: Option<u64>,
    /// Working set size in bytes.
    pub memory_bytes: Option<u64>,
    /// Number of child processes.
    pub child_count: Option<u32>,
}

/// Check if a process is still running.
#[cfg(target_os = "windows")]
pub fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        // Check if process has exited
        let mut exit_code: u32 = 0;
        let still_active =
            windows_sys::Win32::System::Threading::GetExitCodeProcess(handle, &mut exit_code) != 0
                && exit_code == 259; // STILL_ACTIVE = 259
        CloseHandle(handle);
        still_active
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_process_alive(pid: u32) -> bool {
    // On Unix, signal 0 checks if process exists
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Get the total CPU time (kernel + user) for a process in 100-nanosecond intervals.
#[cfg(target_os = "windows")]
pub fn get_cpu_time(pid: u32) -> Option<u64> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }

        let mut creation = MaybeUninit::<FILETIME>::uninit();
        let mut exit = MaybeUninit::<FILETIME>::uninit();
        let mut kernel = MaybeUninit::<FILETIME>::uninit();
        let mut user = MaybeUninit::<FILETIME>::uninit();

        let result = GetProcessTimes(
            handle,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        );

        CloseHandle(handle);

        if result == 0 {
            return None;
        }

        let kernel = kernel.assume_init();
        let user = user.assume_init();

        // Convert FILETIME to u64 (100-nanosecond intervals)
        let kernel_time = (kernel.dwHighDateTime as u64) << 32 | kernel.dwLowDateTime as u64;
        let user_time = (user.dwHighDateTime as u64) << 32 | user.dwLowDateTime as u64;

        Some(kernel_time + user_time)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn get_cpu_time(_pid: u32) -> Option<u64> {
    // On non-Windows, return None (not implemented)
    None
}

/// Get the working set size (memory) for a process in bytes.
#[cfg(target_os = "windows")]
pub fn get_memory_usage(pid: u32) -> Option<u64> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return None;
        }

        let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS>::uninit();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;

        let result = GetProcessMemoryInfo(handle, counters.as_mut_ptr(), size);
        CloseHandle(handle);

        if result == 0 {
            return None;
        }

        let counters = counters.assume_init();
        Some(counters.WorkingSetSize as u64)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn get_memory_usage(_pid: u32) -> Option<u64> {
    None
}

/// Count the number of child processes for a given PID.
#[cfg(target_os = "windows")]
pub fn count_child_processes(pid: u32) -> Option<u32> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut pe32 = MaybeUninit::<PROCESSENTRY32>::uninit();
        (*pe32.as_mut_ptr()).dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;

        let mut count: u32 = 0;

        if Process32First(snapshot, pe32.as_mut_ptr()) != 0 {
            loop {
                let entry = pe32.assume_init_ref();
                if entry.th32ParentProcessID == pid {
                    count += 1;
                }
                if Process32Next(snapshot, pe32.as_mut_ptr()) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
        Some(count)
    }
}

#[cfg(not(target_os = "windows"))]
pub fn count_child_processes(_pid: u32) -> Option<u32> {
    None
}

/// Perform a full health check on a process.
pub fn check_process_health(pid: u32) -> HealthCheckResult {
    let alive = is_process_alive(pid);
    if !alive {
        return HealthCheckResult {
            process_alive: false,
            cpu_time: None,
            memory_bytes: None,
            child_count: None,
        };
    }

    HealthCheckResult {
        process_alive: true,
        cpu_time: get_cpu_time(pid),
        memory_bytes: get_memory_usage(pid),
        child_count: count_child_processes(pid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_process_alive(pid));
    }

    #[test]
    fn test_nonexistent_process() {
        // PID 99999999 is very unlikely to exist
        assert!(!is_process_alive(99999999));
    }

    #[test]
    fn test_cpu_time_current_process() {
        let pid = std::process::id();
        let cpu_time = get_cpu_time(pid);
        // Should be able to query our own process on Windows
        #[cfg(target_os = "windows")]
        assert!(cpu_time.is_some());
        #[cfg(not(target_os = "windows"))]
        let _ = cpu_time; // Non-Windows returns None
    }

    #[test]
    fn test_memory_current_process() {
        let pid = std::process::id();
        let memory = get_memory_usage(pid);
        #[cfg(target_os = "windows")]
        {
            assert!(memory.is_some());
            assert!(memory.unwrap() > 0);
        }
        #[cfg(not(target_os = "windows"))]
        let _ = memory;
    }

    #[test]
    fn test_check_process_health() {
        let pid = std::process::id();
        let result = check_process_health(pid);
        assert!(result.process_alive);
    }

    #[test]
    fn test_dead_process_health() {
        let result = check_process_health(99999999);
        assert!(!result.process_alive);
        assert!(result.cpu_time.is_none());
        assert!(result.memory_bytes.is_none());
        assert!(result.child_count.is_none());
    }
}
