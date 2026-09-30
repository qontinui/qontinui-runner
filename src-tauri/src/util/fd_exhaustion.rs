//! The descriptor-exhaustion stamp: a process-wide record of the last time any
//! wired site hit `EMFILE`/`ENFILE` with the `io::Error` in hand.
//!
//! # Why it exists
//!
//! Plan `2026-09-09-the-pong-receive-path-has-no-liveness-signal-so-fd-
//! exhaustion-still-reads-as-ui-death`. On `merytshost` (2026-09-02) the runner
//! logged 8,267 `Too many open files (os error 24)` and, over the same stretch,
//! ran a webview-recreate ladder to `EXHAUSTED` 240 times — because the
//! `/ui-bridge/pong` that keeps `ui_dead` false arrives on an accepted socket,
//! i.e. needs a descriptor, and nothing told the death verdict so. The ping
//! EMIT succeeded throughout, so the #1384 deliverability gate read
//! `ping_delivered` and let every recreate through.
//!
//! This stamp is one of TWO independent authorities for "this process is out
//! of descriptors" (`verification-and-evidence`
//! `a-control-must-test-the-property-it-names`); the other is the headroom read
//! in the bin's `util::egress_context::fd_headroom`. The stamp is the one that
//! still works at TOTAL exhaustion, where the headroom census itself cannot
//! open `/proc/self/fd`.
//!
//! # Why the lib crate
//!
//! `process_helpers` — where spawn-time exhaustion surfaces first — is compiled
//! into both the lib and the runner bin. A static declared in a shared module
//! is two statics, each seeing half the traffic. So this module is declared
//! ONLY by the lib, and every caller in either crate spells it
//! `qontinui_runner_lib::util::fd_exhaustion`, the same arrangement
//! `wedge_diagnostics` uses for its blocking-pool counter.
//!
//! # Contract
//!
//! Cheap enough for any error path: one `raw_os_error()` read and, only on a
//! match, two relaxed atomic stores. It records; it decides nothing. The
//! decision is `ui_error::classify_fd_pressure`, which is pure and is handed
//! these values.

use std::sync::atomic::{AtomicU64, Ordering};

/// Wall-clock ms of the last `EMFILE`/`ENFILE` any wired site observed.
/// 0 = none ever. A module-level atomic for the reason `PING_EMIT_FAIL_MS` is
/// one: the heartbeat that reads it holds no runtime handle to ask anything.
static FD_EXHAUSTED_MS: AtomicU64 = AtomicU64::new(0);
/// Monotonic count of descriptor-exhaustion errors observed.
static FD_EXHAUSTED_COUNT: AtomicU64 = AtomicU64::new(0);

/// Whether a raw OS error code means "this process (or the system) is out of
/// file descriptors".
///
/// Platform-specific on purpose: 24/23 are `EMFILE`/`ENFILE` only under POSIX.
/// On Windows an `io::Error`'s raw code is a Win32 / Winsock code, where 24 is
/// `ERROR_BAD_LENGTH` and 23 is `ERROR_CRC` — matching those would stamp
/// starvation on unrelated failures and suppress a recovery on no evidence.
pub fn is_fd_exhaustion_code(code: i32) -> bool {
    #[cfg(unix)]
    {
        code == libc::EMFILE || code == libc::ENFILE
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_OPEN_FILES, WSAEMFILE.
        code == 4 || code == 10024
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = code;
        false
    }
}

/// Stamp a descriptor-exhaustion error, if `e` is one. Returns whether it
/// stamped. Call it wherever an `io::Error` from an `open` / `spawn` / `pipe` /
/// `accept` is in hand; an error with no OS code never stamps.
pub fn note_fd_exhaustion(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(code) if is_fd_exhaustion_code(code) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            FD_EXHAUSTED_MS.store(now, Ordering::Relaxed);
            FD_EXHAUSTED_COUNT.fetch_add(1, Ordering::Relaxed);
            true
        }
        _ => false,
    }
}

/// `(exhaustion_errors_total, last_exhausted_ms)`. `last_exhausted_ms == 0`
/// means none has ever been observed — UNKNOWN-shaped history, never an age.
pub fn fd_exhaustion_report() -> (u64, u64) {
    (
        FD_EXHAUSTED_COUNT.load(Ordering::Relaxed),
        FD_EXHAUSTED_MS.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only EMFILE/ENFILE stamp. Any other OS error, and any error with no OS
    /// code at all, must leave the stamp alone — a stamp on an unrelated
    /// failure would suppress UI recovery on no evidence.
    #[cfg(unix)]
    #[test]
    fn note_fd_exhaustion_only_stamps_on_emfile_or_enfile() {
        use std::io::Error;
        assert!(is_fd_exhaustion_code(libc::EMFILE));
        assert!(is_fd_exhaustion_code(libc::ENFILE));
        for code in [libc::ENOENT, libc::EACCES, libc::EAGAIN, libc::ENOMEM, 0] {
            assert!(!is_fd_exhaustion_code(code), "code {code} must not stamp");
            assert!(!note_fd_exhaustion(&Error::from_raw_os_error(code)));
        }
        assert!(!note_fd_exhaustion(&Error::other(
            "Too many open files (but no OS code)"
        )));

        let (count_before, _) = fd_exhaustion_report();
        assert!(note_fd_exhaustion(&Error::from_raw_os_error(libc::EMFILE)));
        assert!(note_fd_exhaustion(&Error::from_raw_os_error(libc::ENFILE)));
        let (count_after, last_ms) = fd_exhaustion_report();
        // `>=`: the counter is process-wide and monotonic, so ours are counted
        // whatever else in this test binary stamps concurrently.
        assert!(count_after >= count_before + 2);
        assert!(last_ms > 0, "a stamp must record a wall-clock time");
    }

    /// Windows codes 24/23 are NOT EMFILE/ENFILE (they are ERROR_BAD_LENGTH /
    /// ERROR_CRC); only the Win32/Winsock exhaustion codes stamp.
    #[cfg(windows)]
    #[test]
    fn windows_fd_exhaustion_codes_are_win32_not_posix() {
        assert!(is_fd_exhaustion_code(4));
        assert!(is_fd_exhaustion_code(10024));
        assert!(!is_fd_exhaustion_code(24));
        assert!(!is_fd_exhaustion_code(23));
    }
}
