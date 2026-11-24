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
//! 3. Breakaway refused (`ERROR_ACCESS_DENIED`) → [`spawn_holder`] answers the
//!    typed [`HolderSpawnError::NeedsWmiFallback`], and the caller re-issues
//!    the spawn through [`spawn_holder_wmi`] (Phase 3): WMI
//!    `Win32_Process.Create`, whose process is created by the WMI provider host
//!    and so sits in none of our jobs. It is the CALLER's step, not a silent
//!    one inside [`spawn_holder`], because the result is a different kind of
//!    process — not our child: no inherited stdout (the holder is told to
//!    write its ready line to a file, `--report`), no `std::process::Child`
//!    (a [`WmiProcess`] handle instead), and the user's default environment
//!    rather than the spawner's allowlist (the holder's own environment is
//!    never the pane's: the child's travels in its spec). WMI is slow next to
//!    `CreateProcess`, and D13 puts it on the new-tab path, so the caller
//!    measures it (`qontinui_pty_holder::spawn::SpawnedHolder::ready_after`).
//!
//! **`CREATE_NEW_PROCESS_GROUP` is locked out.** Windows hands the resulting
//! "ignore Ctrl+C" state to every descendant, which would cost every pane its
//! Ctrl+C. The flag constants below are plain numbers (not `windows-sys`
//! imports) precisely so the tests that lock the bit out run on every OS, not
//! only on a Windows box — one over every route's flags, one over this file's
//! source, so the flag cannot be ORed in beside the route table either.

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
    /// The creation flags this route spawns with: `CreateProcess`'s
    /// `dwCreationFlags` for [`Plain`](Self::Plain) and
    /// [`Breakaway`](Self::Breakaway), `Win32_ProcessStartup.CreateFlags` for
    /// [`Wmi`](Self::Wmi). WMI's process is outside our jobs by construction,
    /// so it needs no breakaway bit, and its window is hidden through
    /// `ShowWindow` rather than `CREATE_NO_WINDOW` (not one of the flags that
    /// class documents).
    pub const fn creation_flags(self) -> u32 {
        match self {
            HolderSpawnRoute::Plain => DETACHED_PROCESS | CREATE_NO_WINDOW,
            HolderSpawnRoute::Breakaway => {
                DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB
            }
            HolderSpawnRoute::Wmi => DETACHED_PROCESS,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HolderSpawnRoute::Plain => "plain",
            HolderSpawnRoute::Breakaway => "breakaway",
            HolderSpawnRoute::Wmi => "wmi",
        }
    }
}

/// Why a holder was not spawned.
#[derive(Debug)]
pub enum HolderSpawnError {
    /// `IsProcessInJob` itself failed, so the route is UNKNOWN. Guessing
    /// "not in a job" would silently spawn a holder the outer job will reap.
    JobQuery(std::io::Error),
    /// The spawner is in a job that refused `CREATE_BREAKAWAY_FROM_JOB`: the
    /// caller must take the WMI route ([`spawn_holder_wmi`]). Typed, so a
    /// refused breakaway is never mistaken for a spawn — and never silently
    /// replaced by a plain spawn the outer job would reap.
    NeedsWmiFallback {
        /// The breakaway `CreateProcess` error.
        breakaway_error: std::io::Error,
    },
    /// `CreateProcess` failed for a reason other than a refused breakaway.
    Spawn(std::io::Error),
    /// `Win32_Process.Create` failed: a COM / WMI error, or a non-zero
    /// `ReturnValue` (2 access denied, 3 insufficient privilege, 8 unknown
    /// failure, 9 path not found, 21 invalid parameter).
    Wmi(String),
    /// WMI reported a process, but the process now behind that pid is not the
    /// holder executable (it exited and the pid was reused before it could be
    /// opened). Nothing was signalled.
    WmiNotOurProcess { pid: u32, detail: String },
}

impl std::fmt::Display for HolderSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HolderSpawnError::JobQuery(e) => write!(f, "IsProcessInJob failed: {e}"),
            HolderSpawnError::NeedsWmiFallback { breakaway_error } => write!(
                f,
                "breakaway refused ({breakaway_error}); the WMI Win32_Process.Create route is needed"
            ),
            HolderSpawnError::Spawn(e) => write!(f, "holder spawn failed: {e}"),
            HolderSpawnError::Wmi(m) => write!(f, "WMI Win32_Process.Create failed: {m}"),
            HolderSpawnError::WmiNotOurProcess { pid, detail } => write!(
                f,
                "WMI reported pid {pid}, but that pid is no longer the holder: {detail}"
            ),
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

    /// `CreateProcess` `cmd` on a [`HolderSpawnRoute::Plain`] or
    /// [`HolderSpawnRoute::Breakaway`] route.
    fn spawn_via(cmd: &mut Command, route: HolderSpawnRoute) -> Result<Child, HolderSpawnError> {
        cmd.creation_flags(route.creation_flags());
        cmd.spawn().map_err(|e| {
            if route == HolderSpawnRoute::Breakaway && e.raw_os_error() == Some(ERROR_ACCESS_DENIED)
            {
                HolderSpawnError::NeedsWmiFallback { breakaway_error: e }
            } else {
                HolderSpawnError::Spawn(e)
            }
        })
    }

    /// Spawn a holder so it survives this process and any job this process
    /// is in. Decides the route from `IsProcessInJob`, never by assumption;
    /// a refused breakaway is [`HolderSpawnError::NeedsWmiFallback`], for the
    /// caller to re-issue through [`super::spawn_holder_wmi`].
    pub fn spawn_holder(cmd: &mut Command) -> Result<(Child, HolderSpawnRoute), HolderSpawnError> {
        let in_job = current_process_in_job().map_err(HolderSpawnError::JobQuery)?;
        let route = if in_job {
            HolderSpawnRoute::Breakaway
        } else {
            HolderSpawnRoute::Plain
        };
        spawn_via(cmd, route).map(|child| (child, route))
    }
}

#[cfg(windows)]
pub use imp::{current_process_in_job, spawn_holder};

#[cfg(windows)]
mod wmi;

#[cfg(windows)]
pub use wmi::{filetime_now, spawn_holder_wmi, WmiProcess};

/// Quote one argument for a Windows command line so `CommandLineToArgvW` (the
/// MSVC CRT's rules, which Rust's own `main` uses) reads it back unchanged.
/// `Win32_Process.Create` takes ONE command-line string, so the WMI route has
/// to build what `std::process::Command` builds for `CreateProcess`. Pure, so
/// it is tested on every OS.
pub fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Backslashes before a quote are doubled, plus one for the quote.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

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
    /// OS because the flags are plain numbers. WMI's startup flags included.
    #[test]
    fn pty_holder_breakaway_no_route_carries_create_new_process_group() {
        for route in ALL_ROUTES {
            assert_eq!(
                route.creation_flags() & CREATE_NEW_PROCESS_GROUP,
                0,
                "{route:?} must never include CREATE_NEW_PROCESS_GROUP"
            );
        }
    }

    /// The same lock-out over this file's SOURCE: the flag may be named only
    /// where it is declared (and in these tests), so it cannot be ORed into a
    /// spawn beside the route table — `cmd.creation_flags(flags | 0x200)`
    /// would pass the route test and fail this one. Comments are ignored.
    /// The value of every integer literal in `line`, in any Rust spelling:
    /// decimal, `0x` / `0o` / `0b`, `_` grouping, a type suffix (`512u32`,
    /// `0x200_usize`). A token that is not a valid literal is skipped.
    fn int_literals(line: &str) -> Vec<u128> {
        const SUFFIXES: [&str; 12] = [
            "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
        ];
        let chars: Vec<char> = line.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let starts = chars[i].is_ascii_digit()
                && (i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_'));
            if !starts {
                i += 1;
                continue;
            }
            let end = (i..chars.len())
                .find(|&j| !(chars[j].is_ascii_alphanumeric() || chars[j] == '_'))
                .unwrap_or(chars.len());
            let token: String = chars[i..end]
                .iter()
                .filter(|c| **c != '_')
                .collect::<String>()
                .to_ascii_lowercase();
            i = end;
            let rest = token.get(2..).unwrap_or("");
            let (radix, body) = match token.get(..2) {
                Some("0x") => (16, rest),
                Some("0o") => (8, rest),
                Some("0b") => (2, rest),
                _ => (10, token.as_str()),
            };
            let digits_end = body
                .find(|c: char| !c.is_digit(radix))
                .unwrap_or(body.len());
            let (digits, suffix) = body.split_at(digits_end);
            if digits.is_empty() || !(suffix.is_empty() || SUFFIXES.contains(&suffix)) {
                continue;
            }
            if let Ok(v) = u128::from_str_radix(digits, radix) {
                out.push(v);
            }
        }
        out
    }

    /// Whether `line` spells the `CREATE_NEW_PROCESS_GROUP` bit by value: any
    /// integer literal equal to 0x200, or the shift `1 << 9`.
    fn names_0x200(line: &str) -> bool {
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        let shift = compact.match_indices("1<<9").any(|(i, _)| {
            let wordish =
                |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            !wordish(compact.get(..i).and_then(|b| b.chars().last()))
                && !wordish(compact.get(i + 4..).and_then(|a| a.chars().next()))
        });
        shift || int_literals(line).contains(&0x200)
    }

    #[test]
    fn pty_holder_breakaway_names_0x200_in_every_spelling() {
        for hit in [
            "flags | 0x200)",
            "0x0000_0200",
            "flags | 512",
            "512u32",
            "512_u32",
            "0x200_usize",
            "0b10_0000_0000",
            "0o1000",
            "flags | (1 << 9)",
            "1<<9",
        ] {
            assert!(names_0x200(hit), "{hit}");
        }
        for miss in [
            "0x2000",
            "5120",
            "x512",
            "1 << 90",
            "0x0100_0000",
            "512x",
            "v512",
        ] {
            assert!(!names_0x200(miss), "{miss}");
        }
    }

    #[test]
    fn pty_holder_breakaway_source_never_ors_in_create_new_process_group() {
        // `lines()` drops a trailing `\r` too, so a CRLF checkout scans the
        // same way. Only this file's code before its test module is scanned.
        let src: Vec<&str> = include_str!("holder_spawn.rs").lines().collect();
        let tests_at = src
            .windows(2)
            .position(|w| w[0].trim() == "#[cfg(test)]" && w[1].trim() == "mod tests {")
            .expect("the tests module");
        let wmi: Vec<&str> = include_str!("holder_spawn/wmi.rs").lines().collect();
        for (file, text) in [
            ("holder_spawn.rs", &src[..tests_at]),
            ("holder_spawn/wmi.rs", &wmi[..]),
        ] {
            for (n, line) in text.iter().enumerate() {
                let line = line.split("//").next().unwrap_or("");
                let declaration = line.contains("pub const CREATE_NEW_PROCESS_GROUP: u32");
                let lower = line.to_ascii_lowercase();
                let mentions = line.contains("CREATE_NEW_PROCESS_GROUP") || names_0x200(&lower);
                assert!(
                    declaration || !mentions,
                    "{file}:{}: CREATE_NEW_PROCESS_GROUP reaches a holder spawn: {line}",
                    n + 1
                );
            }
        }
    }

    #[test]
    fn pty_holder_breakaway_every_route_is_detached() {
        for route in ALL_ROUTES {
            assert_ne!(route.creation_flags() & DETACHED_PROCESS, 0, "{route:?}");
        }
        for route in [HolderSpawnRoute::Plain, HolderSpawnRoute::Breakaway] {
            assert_ne!(route.creation_flags() & CREATE_NO_WINDOW, 0, "{route:?}");
        }
    }

    #[test]
    fn pty_holder_breakaway_only_the_breakaway_route_breaks_away() {
        for route in ALL_ROUTES {
            assert_eq!(
                route.creation_flags() & CREATE_BREAKAWAY_FROM_JOB != 0,
                route == HolderSpawnRoute::Breakaway,
                "{route:?}"
            );
        }
    }

    /// Round-trip through the MSVC rules a holder's `main` parses with.
    #[test]
    fn pty_holder_breakaway_wmi_command_line_quoting() {
        assert_eq!(quote_windows_arg("plain"), "plain");
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(
            quote_windows_arg(r"C:\Program Files\q\qontinui-pty-holder.exe"),
            r#""C:\Program Files\q\qontinui-pty-holder.exe""#
        );
        // A trailing backslash inside quotes is doubled, so it cannot escape
        // the closing quote.
        assert_eq!(quote_windows_arg(r"C:\a dir\"), r#""C:\a dir\\""#);
        assert_eq!(quote_windows_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_windows_arg(r#"a\"b"#), r#""a\\\"b""#);
        // No quoting needed: backslashes are literal outside quotes.
        assert_eq!(quote_windows_arg(r"C:\dir\"), r"C:\dir\");
    }

    /// The hand-written numbers must equal the SDK's. Windows-only because
    /// `windows-sys` is a target-gated dependency.
    #[cfg(windows)]
    #[test]
    fn pty_holder_breakaway_flag_constants_match_windows_sys() {
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
