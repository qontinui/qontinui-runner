//! What counts as a TRANSIENT failure to launch a program, and the bounded
//! retry both spawners use — the runner's headless helper
//! (`src-tauri/src/claude_cli_spawn.rs`) and the holder's own child spawn
//! ([`crate::pty::Pane::spawn`]).
//!
//! Plan `2026-10-03-runner-claude-spawn-fails-enoent-during-cli-auto-update`
//! wrote the classification for the runner's headless spawns. Plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`
//! D7 / Phase 5b moved it HERE, because the holder's child spawn hit the same
//! reinstall window (10 of the 11 nawa in-process fallbacks of
//! 2026-11-13..23 were `Unable to spawn .../bin/claude because it doesn't
//! exist`) and this crate cannot depend on the runner crate. One definition,
//! two spawners: the runner re-exports [`transient_kind`].
//!
//! The Claude Code CLI's auto-updater, on an npm-global install, renames
//! `bin/claude` and the package dir aside before reinstalling them, so for
//! about 2 s `claude` is on no PATH entry (`ENOENT`); the native binary is
//! written in place, so the same window yields `EACCES` (mode bit not yet set),
//! `ENOEXEC` (partly written) and `ETXTBSY` (still open for writing).
//!
//! Descriptor exhaustion (`EMFILE` / `ENFILE`) is deliberately NOT transient: a
//! retry inside the same process changes nothing it can see. It is a hard
//! error, but [`failure_cause`] names it, so the caller surfaces it as such
//! instead of as an anonymous spawn failure (D7 (2b)).

use std::io;
use std::time::{Duration, Instant};

/// Back-off before each RETRY. `len() + 1` attempts in all; the sum (10 s) is
/// the most a spawn waits before reporting failure — several times the measured
/// ~2 s reinstall window, and well inside the runner's 30 s wait for a holder's
/// ready line (`spawn::DEFAULT_REPORT_TIMEOUT`).
pub const SPAWN_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(6),
];

/// The short kind token for a spawn error that a CLI reinstall in progress can
/// produce, or `None` when the error is not one of those.
///
/// It matches the OS code first and falls back to the portable
/// [`io::ErrorKind`]. That fallback is never used for ENOEXEC or ETXTBSY,
/// which have no stable `ErrorKind` of their own. Windows `193`
/// (`ERROR_BAD_EXE_FORMAT`) is deliberately NOT transient. On the runner it
/// means a direct `CreateProcessW` of the extensionless identity shim, which
/// is a deterministic defect that a retry would only repeat.
pub fn transient_kind(e: &io::Error) -> Option<&'static str> {
    if let Some(code) = e.raw_os_error() {
        // A raw code we did not list is NOT transient: the portable
        // `ErrorKind` fallback below would otherwise sweep in, e.g., `EPERM`
        // (a `setpgid` failure) as `PermissionDenied`.
        #[cfg(unix)]
        {
            return match code {
                libc::ENOENT => Some("enoent"),
                libc::EACCES => Some("eacces"),
                libc::ENOEXEC => Some("enoexec"),
                libc::ETXTBSY => Some("etxtbsy"),
                _ => None,
            };
        }
        #[cfg(windows)]
        {
            return match code {
                // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
                2 | 3 => Some("enoent"),
                // ERROR_ACCESS_DENIED
                5 => Some("eacces"),
                // ERROR_SHARING_VIOLATION: the exe is still being written.
                32 => Some("sharing_violation"),
                _ => None,
            };
        }
        // A target with no table above falls through to the kind check.
        #[cfg(not(any(unix, windows)))]
        let _ = code;
    }
    // No raw code (e.g. std's own "program not found" on Windows): the kind.
    match e.kind() {
        io::ErrorKind::NotFound => Some("enoent"),
        io::ErrorKind::PermissionDenied => Some("eacces"),
        _ => None,
    }
}

/// `true` for a process or system descriptor-table exhaustion.
pub fn is_fd_exhaustion(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(c) if c == libc::EMFILE || c == libc::ENFILE)
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_OPEN_FILES
        matches!(e.raw_os_error(), Some(4))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// The one-word cause a spawn failure is reported under: a [`transient_kind`]
/// token, `emfile` for descriptor exhaustion, else `other`.
///
/// It travels in the holder's `holder_error=child cause=<token> …` line, which
/// is how the runner learns WHY a pane fell back to an in-process PTY without
/// parsing prose.
pub fn failure_cause(e: &io::Error) -> &'static str {
    if let Some(kind) = transient_kind(e) {
        kind
    } else if is_fd_exhaustion(e) {
        "emfile"
    } else {
        "other"
    }
}

/// One failed attempt: the typed error the classifier reads, and the text a
/// report carries (which may say more than the bare OS error, e.g. the path).
#[derive(Debug)]
pub struct AttemptError {
    pub error: io::Error,
    pub text: String,
}

impl From<io::Error> for AttemptError {
    fn from(error: io::Error) -> Self {
        AttemptError {
            text: error.to_string(),
            error,
        }
    }
}

/// A spawn that produced no child after [`retry_transient_sync`].
#[derive(Debug)]
pub struct SpawnFailure {
    pub last: AttemptError,
    pub attempts: u32,
    pub elapsed: Duration,
    /// [`failure_cause`] of the last error.
    pub cause: &'static str,
}

impl SpawnFailure {
    /// The failure as an `io::Error` of the last error's kind, its text first
    /// (so a reader anchored on `Unable to spawn …` still matches) and then
    /// `[cause=<token> attempts=<n> over <s>s]`.
    pub fn into_io_error(self) -> io::Error {
        let kind = self.last.error.kind();
        io::Error::new(
            kind,
            format!(
                "{} [cause={} attempts={} over {:.1}s]",
                self.last.text,
                self.cause,
                self.attempts,
                self.elapsed.as_secs_f64()
            ),
        )
    }
}

/// Run `attempt` until it succeeds, fails with an error [`transient_kind`] does
/// not accept, or `delays` is spent. Synchronous — the holder has no runtime —
/// with `sleep` injected so the policy is testable without a clock.
pub fn retry_transient_sync<T>(
    delays: &[Duration],
    mut attempt: impl FnMut() -> Result<T, AttemptError>,
    mut sleep: impl FnMut(Duration),
) -> Result<T, SpawnFailure> {
    let started = Instant::now();
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let last = match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let retry_index = (attempts - 1) as usize;
        match (transient_kind(&last.error), delays.get(retry_index)) {
            (Some(_), Some(&delay)) => sleep(delay),
            _ => {
                let cause = failure_cause(&last.error);
                return Err(SpawnFailure {
                    last,
                    attempts,
                    elapsed: started.elapsed(),
                    cause,
                });
            }
        }
    }
}

/// The `cause=<token>` a `holder_error=child …` line carries, or `None` when
/// the line has none (a holder built before Phase 5b).
pub fn cause_in_report(line: &str) -> Option<&str> {
    line.split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .find_map(|w| w.strip_prefix("cause="))
        .filter(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn os(code: i32) -> io::Error {
        io::Error::from_raw_os_error(code)
    }

    #[cfg(unix)]
    #[test]
    fn the_reinstall_shapes_are_transient_and_fd_exhaustion_is_not() {
        assert_eq!(transient_kind(&os(libc::ENOENT)), Some("enoent"));
        assert_eq!(transient_kind(&os(libc::EACCES)), Some("eacces"));
        assert_eq!(transient_kind(&os(libc::ENOEXEC)), Some("enoexec"));
        assert_eq!(transient_kind(&os(libc::ETXTBSY)), Some("etxtbsy"));
        assert_eq!(transient_kind(&os(libc::EMFILE)), None);
        assert_eq!(transient_kind(&os(libc::EPERM)), None);
        assert_eq!(failure_cause(&os(libc::EMFILE)), "emfile");
        assert_eq!(failure_cause(&os(libc::ENFILE)), "emfile");
        assert_eq!(failure_cause(&os(libc::ETXTBSY)), "etxtbsy");
        assert_eq!(failure_cause(&os(libc::EPERM)), "other");
    }

    #[cfg(unix)]
    #[test]
    fn a_transient_window_is_ridden_out_and_emfile_is_one_attempt() {
        let mut calls = 0;
        let mut slept = Vec::new();
        let got = retry_transient_sync(
            &SPAWN_RETRY_DELAYS,
            || {
                calls += 1;
                if calls < 3 {
                    Err(AttemptError::from(os(libc::ETXTBSY)))
                } else {
                    Ok(calls)
                }
            },
            |d| slept.push(d),
        );
        assert_eq!(got.unwrap(), 3);
        assert_eq!(slept, SPAWN_RETRY_DELAYS[..2].to_vec());

        let mut calls = 0;
        let err = retry_transient_sync::<()>(
            &SPAWN_RETRY_DELAYS,
            || {
                calls += 1;
                Err(AttemptError::from(os(libc::EMFILE)))
            },
            |_| panic!("EMFILE must not be retried"),
        )
        .unwrap_err();
        assert_eq!((calls, err.attempts, err.cause), (1, 1, "emfile"));
        let text = err.into_io_error().to_string();
        assert!(text.contains("[cause=emfile attempts=1"), "{text}");
        assert_eq!(cause_in_report(&text), Some("emfile"));
    }

    #[test]
    fn a_report_without_a_cause_reads_none() {
        assert_eq!(
            cause_in_report("holder_error=child child: spawn: boom"),
            None
        );
        assert_eq!(
            cause_in_report("holder_error=child child: x [cause=enoent attempts=4 over 10.0s]"),
            Some("enoent")
        );
    }
}
