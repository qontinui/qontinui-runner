//! How a PTY holder is spawned on Windows so that it outlives its spawner.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! D4 as corrected on 2026-09-27. Written in Phase 0 (survival spike); the
//! runner's holder spawner (`qontinui_pty_holder::spawn`) calls it since Phase 2.
//!
//! **Which job kills a holder.** The runner never places *itself* in its own
//! `KILL_ON_JOB_CLOSE` job (`job_object::init_job_object` only creates it; PTY
//! children are enrolled one at a time), so a holder the runner spawns is
//! outside that job simply by not enrolling it. What does kill it is an
//! OUTER job the runner itself sits in — a supervisor-spawned runner is in the
//! supervisor's job, and a user's launcher may put it in one — because since
//! Windows 8 a child joins every job its parent is in unless it breaks away.
//!
//! So the route is decided by an observable, not assumed:
//!
//! 1. `IsProcessInJob(GetCurrentProcess())` is false → [`HolderSpawnRoute::Plain`]:
//!    `DETACHED_PROCESS` (plus a redundant `CREATE_NO_WINDOW`, which Windows
//!    ignores alongside it), no special machinery.
//! 2. In a job → [`HolderSpawnRoute::Breakaway`]: add `CREATE_BREAKAWAY_FROM_JOB`,
//!    legal only when every enclosing job sets `JOB_OBJECT_LIMIT_BREAKAWAY_OK`.
//! 3. Breakaway refused (`ERROR_ACCESS_DENIED`) → WMI `Win32_Process.Create`,
//!    whose child is created by the WMI provider host and so sits in none of
//!    our jobs. **Not built in Phase 0**: it is returned as the typed
//!    [`HolderSpawnError::NeedsWmiFallback`] so a caller can never mistake the
//!    missing arm for a successful spawn. A WMI-spawned holder also has no
//!    inherited stdout, which is why the holder accepts `--report-file`.
//!
//! **`CREATE_NEW_PROCESS_GROUP` is locked out.** Windows hands the resulting
//! "ignore Ctrl+C" state to every descendant, which would cost every pane its
//! Ctrl+C. The flag constants below are plain numbers (not `windows-sys`
//! imports) precisely so the test that locks the bit out runs on every OS, not
//! only on a Windows box.

/// `DETACHED_PROCESS` — the holder gets no console of its own.
pub const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP` — declared only so tests can prove it is ABSENT
/// from every flag set this module produces. Never OR it in.
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// `CREATE_BREAKAWAY_FROM_JOB` — leave every enclosing job that allows it.
pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
/// `CREATE_NO_WINDOW`. **Ignored whenever `DETACHED_PROCESS` is also set**
/// (documented `CreateProcess` behaviour), so in every route below it is
/// redundant: `DETACHED_PROCESS` alone is what keeps the holder console-less.
/// It stays in the flag sets only as a no-op belt should a later change drop
/// `DETACHED_PROCESS`; nothing may rely on it while that flag is present.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Win32 `ERROR_ACCESS_DENIED`: what `CreateProcess` answers when a
/// breakaway is requested from a job that does not set `BREAKAWAY_OK`.
pub const ERROR_ACCESS_DENIED: i32 = 5;

/// Which spawn route produced (or would produce) a holder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HolderSpawnRoute {
    /// The spawner is in no job: a detached process is enough.
    Plain,
    /// The spawner is in a job that permits breakaway.
    Breakaway,
    /// The spawner is in a job that forbids breakaway: WMI `Win32_Process.Create`.
    Wmi,
}

impl HolderSpawnRoute {
    /// The `CreateProcess` creation flags for this route, or `None` for WMI,
    /// which does not go through `CreateProcess` at all.
    pub const fn creation_flags(self) -> Option<u32> {
        match self {
            HolderSpawnRoute::Plain => Some(DETACHED_PROCESS | CREATE_NO_WINDOW),
            HolderSpawnRoute::Breakaway => {
                Some(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
            }
            HolderSpawnRoute::Wmi => None,
        }
    }

    /// Parse a route's argv spelling (the deleted Phase 0 spike's parent arm
    /// used it; kept for the Phase 3 Windows survival tests).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "plain" => Some(HolderSpawnRoute::Plain),
            "breakaway" => Some(HolderSpawnRoute::Breakaway),
            "wmi" => Some(HolderSpawnRoute::Wmi),
            _ => None,
        }
    }
}

/// Why a holder was not spawned.
#[derive(Debug)]
pub enum HolderSpawnError {
    /// `IsProcessInJob` itself failed, so the route is UNKNOWN. Guessing
    /// "not in a job" would silently spawn a holder the outer job will reap.
    JobQuery(std::io::Error),
    /// The spawner is in a job that refused `CREATE_BREAKAWAY_FROM_JOB`
    /// (or the WMI route was asked for explicitly). The WMI
    /// `Win32_Process.Create` fallback is NOT implemented in Phase 0 — this
    /// variant is the explicit hole, never a silent plain spawn.
    NeedsWmiFallback {
        /// The breakaway `CreateProcess` error, when breakaway was tried.
        breakaway_error: Option<std::io::Error>,
    },
    /// `CreateProcess` failed for a reason other than a refused breakaway.
    Spawn(std::io::Error),
}

impl std::fmt::Display for HolderSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HolderSpawnError::JobQuery(e) => write!(f, "IsProcessInJob failed: {e}"),
            HolderSpawnError::NeedsWmiFallback { breakaway_error } => match breakaway_error {
                Some(e) => write!(
                    f,
                    "breakaway refused ({e}); WMI Win32_Process.Create fallback not implemented (Phase 0)"
                ),
                None => write!(
                    f,
                    "WMI Win32_Process.Create fallback not implemented (Phase 0)"
                ),
            },
            HolderSpawnError::Spawn(e) => write!(f, "holder spawn failed: {e}"),
        }
    }
}

impl std::error::Error for HolderSpawnError {}

#[cfg(windows)]
mod imp {
    use super::{HolderSpawnError, HolderSpawnRoute, ERROR_ACCESS_DENIED};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use windows_sys::Win32::Foundation::{BOOL, HANDLE};
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// Whether THIS process is in any job object.
    pub fn current_process_in_job() -> std::io::Result<bool> {
        let mut in_job: BOOL = 0;
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // close; a null job handle asks "in ANY job"; `in_job` is a valid
        // out-pointer for the duration of the call.
        let ok = unsafe {
            IsProcessInJob(
                GetCurrentProcess(),
                std::ptr::null_mut::<core::ffi::c_void>() as HANDLE,
                &mut in_job,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(in_job != 0)
    }

    /// Spawn `cmd` by an explicitly chosen route — for tests that exercise
    /// each arm on its own (Phase 3); production code calls [`spawn_holder`].
    pub fn spawn_holder_via(
        cmd: &mut Command,
        route: HolderSpawnRoute,
    ) -> Result<Child, HolderSpawnError> {
        let Some(flags) = route.creation_flags() else {
            return Err(HolderSpawnError::NeedsWmiFallback {
                breakaway_error: None,
            });
        };
        cmd.creation_flags(flags);
        cmd.spawn().map_err(|e| {
            if route == HolderSpawnRoute::Breakaway && e.raw_os_error() == Some(ERROR_ACCESS_DENIED)
            {
                HolderSpawnError::NeedsWmiFallback {
                    breakaway_error: Some(e),
                }
            } else {
                HolderSpawnError::Spawn(e)
            }
        })
    }

    /// Spawn a holder so it survives this process and any job this process
    /// is in. Decides the route from `IsProcessInJob`, never by assumption.
    pub fn spawn_holder(cmd: &mut Command) -> Result<(Child, HolderSpawnRoute), HolderSpawnError> {
        let in_job = current_process_in_job().map_err(HolderSpawnError::JobQuery)?;
        let route = if in_job {
            HolderSpawnRoute::Breakaway
        } else {
            HolderSpawnRoute::Plain
        };
        spawn_holder_via(cmd, route).map(|child| (child, route))
    }
}

#[cfg(windows)]
pub use imp::{current_process_in_job, spawn_holder, spawn_holder_via};

/// Test support: an OUTER `KILL_ON_JOB_CLOSE` job, the shape of the
/// supervisor's job that reaps the runners it spawns. `breakaway_ok` adds
/// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, which is what decides whether route 2 of
/// the module docs is legal. Closing (dropping) it kills every process still
/// in it.
#[cfg(windows)]
pub struct OuterKillOnCloseJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl OuterKillOnCloseJob {
    pub fn create(breakaway_ok: bool) -> std::io::Result<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: plain Win32 calls with valid (null or owned) arguments; the
        // handle is closed on every failure path and owned by Self otherwise.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | if breakaway_ok {
                    JOB_OBJECT_LIMIT_BREAKAWAY_OK
                } else {
                    0
                };
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(e);
            }
            Ok(Self(handle))
        }
    }

    /// Put `child` in this job.
    pub fn assign(&self, child: &std::process::Child) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        // SAFETY: `child` owns a live process handle for the duration of the call.
        let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as _) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for OuterKillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed exactly once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_ROUTES: [HolderSpawnRoute; 3] = [
        HolderSpawnRoute::Plain,
        HolderSpawnRoute::Breakaway,
        HolderSpawnRoute::Wmi,
    ];

    /// The lock-out D4 requires: no route may ever carry
    /// `CREATE_NEW_PROCESS_GROUP`, or every pane loses Ctrl+C. Runs on every
    /// OS because the flags are plain numbers.
    #[test]
    fn no_holder_route_carries_create_new_process_group() {
        for route in ALL_ROUTES {
            if let Some(flags) = route.creation_flags() {
                assert_eq!(
                    flags & CREATE_NEW_PROCESS_GROUP,
                    0,
                    "{route:?} must never include CREATE_NEW_PROCESS_GROUP"
                );
            }
        }
    }

    #[test]
    fn every_createprocess_route_is_detached_and_windowless() {
        for route in [HolderSpawnRoute::Plain, HolderSpawnRoute::Breakaway] {
            let flags = route.creation_flags().unwrap();
            assert_ne!(flags & DETACHED_PROCESS, 0, "{route:?}");
            assert_ne!(flags & CREATE_NO_WINDOW, 0, "{route:?}");
        }
    }

    #[test]
    fn only_the_breakaway_route_breaks_away() {
        assert_eq!(
            HolderSpawnRoute::Plain.creation_flags().unwrap() & CREATE_BREAKAWAY_FROM_JOB,
            0
        );
        assert_ne!(
            HolderSpawnRoute::Breakaway.creation_flags().unwrap() & CREATE_BREAKAWAY_FROM_JOB,
            0
        );
        assert_eq!(HolderSpawnRoute::Wmi.creation_flags(), None);
    }

    #[test]
    fn route_parse_round_trips() {
        assert_eq!(
            HolderSpawnRoute::parse("plain"),
            Some(HolderSpawnRoute::Plain)
        );
        assert_eq!(
            HolderSpawnRoute::parse("breakaway"),
            Some(HolderSpawnRoute::Breakaway)
        );
        assert_eq!(HolderSpawnRoute::parse("wmi"), Some(HolderSpawnRoute::Wmi));
        assert_eq!(HolderSpawnRoute::parse("group"), None);
    }

    /// The hand-written numbers must equal the SDK's. Windows-only because
    /// `windows-sys` is a target-gated dependency.
    #[cfg(windows)]
    #[test]
    fn flag_constants_match_windows_sys() {
        use windows_sys::Win32::System::Threading as t;
        assert_eq!(DETACHED_PROCESS, t::DETACHED_PROCESS);
        assert_eq!(CREATE_NEW_PROCESS_GROUP, t::CREATE_NEW_PROCESS_GROUP);
        assert_eq!(CREATE_BREAKAWAY_FROM_JOB, t::CREATE_BREAKAWAY_FROM_JOB);
        assert_eq!(CREATE_NO_WINDOW, t::CREATE_NO_WINDOW);
        assert_eq!(
            ERROR_ACCESS_DENIED as u32,
            windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED
        );
    }
}
