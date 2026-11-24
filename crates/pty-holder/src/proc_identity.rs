//! Process identity beyond the pid: what lets a boot sweep tell a pane's
//! recorded child from an unrelated process that later got the same pid.
//!
//! Plan `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`,
//! Phase 3 (D8). A pid is a NAME, recycled once its process is reaped. A holder
//! therefore records, beside its child's pid, the child's START TOKEN — a value
//! the OS fixes when the process is created and that a recycled pid does not
//! reproduce:
//!
//! - Linux: `linux:<boot id>:<start time in clock ticks since boot>`
//!   (`/proc/sys/kernel/random/boot_id`, field 22 of `/proc/<pid>/stat`). The
//!   boot id makes a token from a previous boot never match.
//! - Windows: `windows:<creation FILETIME>` (`GetProcessTimes`), which is
//!   absolute and so needs no boot id.
//! - Elsewhere (macOS): no token. A sweep there never signals a recorded pid
//!   it cannot verify; it reports it instead.
//!
//! On Linux this module also reads the `/proc` fields the reapers need
//! ([`ProcStat`]) and lists pids.
//!
//! DATA-PATH-adjacent: `source_guard` bans text decoding here too.

/// The start token of `pid`, or `None` when it does not exist, cannot be read,
/// or this OS has no token.
pub fn start_token(pid: u32) -> Option<String> {
    imp::start_token(pid)
}

/// Whether `token` was taken during THIS boot. A Linux token carries its boot
/// id. A Windows token is an absolute creation time, compared against this
/// boot's start ([`boot_time`]; review round 3, R5). Anything unreadable
/// answers `true`, since nothing then says otherwise.
pub fn token_is_from_this_boot(token: &str) -> bool {
    #[cfg(target_os = "linux")]
    if let Some(rest) = token.strip_prefix("linux:") {
        return match (imp::boot_id(), rest.split_once(':')) {
            (Some(now), Some((then, _))) => now == then,
            _ => true,
        };
    }
    if let (Some(created), Some(boot)) = (windows_token_time(token), boot_time()) {
        return created >= boot;
    }
    true
}

/// The creation instant a `windows:<FILETIME>` token names. Pure, so it is
/// tested on every OS.
fn windows_token_time(token: &str) -> Option<std::time::SystemTime> {
    /// FILETIME units (100 ns) between 1601-01-01 and the Unix epoch.
    const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
    let filetime: u64 = token.strip_prefix("windows:")?.parse().ok()?;
    let since_unix = filetime.checked_sub(FILETIME_UNIX_EPOCH)?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(since_unix.checked_mul(100)?))
}

/// When this host booted, or `None` where that is not readable (macOS).
pub fn boot_time() -> Option<std::time::SystemTime> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let secs: u64 = stat
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
    }
    #[cfg(windows)]
    {
        // SAFETY: GetTickCount64 takes no arguments and cannot fail.
        let up = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        std::time::SystemTime::now().checked_sub(std::time::Duration::from_millis(up))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

/// Whether `pid` is alive right now AND is the process `token` was taken from.
/// `false` for an absent token: unverifiable is never "verified".
pub fn is_same_process(pid: u32, token: Option<&str>) -> bool {
    match (token, start_token(pid)) {
        (Some(want), Some(have)) => want == have,
        _ => false,
    }
}

/// The `/proc/<pid>/stat` fields the Linux reapers read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// State `Z`: exited, not yet reaped by its parent. A signal changes
    /// nothing about it, so reapers skip it.
    pub zombie: bool,
    pub ppid: i32,
    pub pgrp: i32,
    pub session: i32,
    /// Field 22, clock ticks after boot.
    pub start_ticks: u64,
}

/// Parse the body of a `/proc/<pid>/stat` file. The comm field (2) is
/// parenthesised and may itself contain `)` or spaces, so every field this
/// reads is counted from after the LAST `)`.
pub fn parse_stat(raw: &str) -> Option<ProcStat> {
    let (_, rest) = raw.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    // After the comm: state(0) ppid(1) pgrp(2) session(3) … starttime(19).
    Some(ProcStat {
        zombie: *f.first()? == "Z",
        ppid: f.get(1)?.parse().ok()?,
        pgrp: f.get(2)?.parse().ok()?,
        session: f.get(3)?.parse().ok()?,
        start_ticks: f.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{parse_stat, ProcStat};

    /// This boot's id, read once.
    pub fn boot_id() -> Option<&'static str> {
        static BOOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        BOOT.get_or_init(|| {
            std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .as_deref()
    }

    pub fn stat(pid: u32) -> Option<ProcStat> {
        parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    pub fn start_token(pid: u32) -> Option<String> {
        let st = stat(pid)?;
        Some(format!("linux:{}:{}", boot_id()?, st.start_ticks))
    }

    /// Every pid in `/proc`, unordered. A process that exits while this runs
    /// may or may not be listed; callers re-verify each pid they act on.
    pub fn list_pids() -> Vec<u32> {
        std::fs::read_dir("/proc")
            .map(|rd| {
                rd.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(target_os = "linux")]
pub use imp::{list_pids, stat};

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `GetExitCodeProcess`'s answer for a process that has not exited
    /// (`STILL_ACTIVE`, 259).
    const STILL_ACTIVE: u32 = 259;

    /// The creation time of the process behind `handle` — ONE implementation,
    /// in runner-win32, shared with the pane-tree kill.
    pub use qontinui_runner_win32::process_tree::creation_time;

    pub fn start_token(pid: u32) -> Option<String> {
        // SAFETY: OpenProcess takes no pointers; a non-null result is a handle
        // owned here and closed below.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return None;
        }
        // SAFETY: `h` was just opened with PROCESS_QUERY_LIMITED_INFORMATION.
        let created = unsafe { creation_time(h) };
        // A Windows process object outlives the process while ANY handle to it
        // is open, anywhere (our own `Child`, conhost, a scanner), and stays
        // readable. Without this check an exited process kept its token, so
        // `is_same_process` called it alive. A process that genuinely exits
        // with code 259 is indistinguishable and reads as alive. That is safe
        // for every caller: liveness waits stay bounded, and `verify_holder`'s
        // terminate still needs the same creation time, so it can only reach
        // the same (already-exited) process object, where it is a no-op.
        let mut code: u32 = 0;
        // SAFETY: `h` carries PROCESS_QUERY_LIMITED_INFORMATION, which
        // GetExitCodeProcess requires; `code` is a valid local.
        let running = unsafe { GetExitCodeProcess(h, &mut code) } != 0 && code == STILL_ACTIVE;
        // SAFETY: closing the handle OpenProcess returned, exactly once.
        unsafe { CloseHandle(h) };
        if !running {
            return None;
        }
        created.map(|c| format!("windows:{c}"))
    }
}

#[cfg(windows)]
pub use imp::creation_time;

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn start_token(_pid: u32) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review round 3, R5: a Windows token's creation time is an absolute
    /// instant, readable on any OS.
    #[test]
    fn pty_holder_reap_windows_token_time_reads_the_filetime() {
        // 2021-01-01T00:00:00Z as FILETIME.
        let t = windows_token_time("windows:132539328000000000").unwrap();
        assert_eq!(
            t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            1_609_459_200
        );
        assert_eq!(windows_token_time("linux:boot:5"), None);
        assert_eq!(windows_token_time("windows:1"), None, "before 1970");
        assert_eq!(windows_token_time("windows:x"), None);
    }

    #[test]
    fn pty_holder_reap_parse_stat_counts_fields_after_the_last_paren() {
        let raw = "1234 (we(ird) name) S 1 1234 1234 34816 1234 4194560 100 0 0 0 1 2 0 0 \
                   20 0 1 0 987654 1000 200";
        assert_eq!(
            parse_stat(raw),
            Some(ProcStat {
                zombie: false,
                ppid: 1,
                pgrp: 1234,
                session: 1234,
                start_ticks: 987654,
            })
        );
        assert!(
            parse_stat("9 (z) Z 1 9 9 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5")
                .unwrap()
                .zombie
        );
        assert_eq!(parse_stat("no paren here"), None);
        assert_eq!(parse_stat("1 (x) S 1 2"), None, "truncated stat");
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn pty_holder_reap_start_token_tells_a_live_process_from_a_reaped_one() {
        let me = std::process::id();
        let mine = start_token(me).expect("this process has a token");
        assert!(is_same_process(me, Some(&mine)));
        assert!(!is_same_process(me, Some("linux:other-boot:1")));
        assert!(!is_same_process(me, None), "no token is never verified");

        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let pid = child.id();
        // Not yet waited on: the child (or its zombie) still has a token.
        let token = start_token(pid).expect("a just-spawned child has a token");
        child.wait().unwrap();
        // Windows keeps the process object while any handle is open, and the
        // `Child` holds one until it is dropped.
        drop(child);
        // Reaped: the pid now names nothing, or a different process.
        assert_ne!(
            start_token(pid).as_deref(),
            Some(token.as_str()),
            "a reaped pid kept its token"
        );
    }
}
