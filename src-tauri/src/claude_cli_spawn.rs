//! Spawning the Claude CLI as a headless `tokio::process` child, with
//! transient launch failures retried in-process.
//!
//! Plan `2026-10-03-runner-claude-spawn-fails-enoent-during-cli-auto-update`.
//!
//! The Claude Code CLI's own auto-updater, on an npm-global install, runs
//! `npm install --global @anthropic-ai/claude-code@<version>` as often as about
//! once a minute on a busy box, even when that version is already installed.
//! npm's reify step RENAMES `bin/claude` and the package dir aside before it
//! reinstalls them. That leaves a window of about 2 s in which `claude` is on no
//! PATH entry (`ENOENT`). The native binary the package ships is also written in
//! place, so the same window yields `EACCES` (the file exists before its mode
//! bit is set) and `ENOEXEC` (the file is only partly written). All three were
//! measured in the runner's logs. A spawn that lands in the window used to fail
//! once and be final: a scheduler task's tick, a headless gate continuation, or
//! the first attempt of a coord agent spawn.
//!
//! This module is the one place that decides what counts as transient for a CLI
//! spawn, and it retries those errors with a short, bounded back-off. Its
//! budget is [`RETRY_DELAYS`]: 4 attempts over about 10 s, several times longer
//! than the measured window. Any other error returns at once.
//!
//! In-process by design. A user's runner has no supervisor, so recovery a user
//! gets has to live here.

use std::future::Future;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use tracing::warn;

/// Back-off before each RETRY. `len() + 1` attempts in all; the sum (10 s) is
/// the most a spawn waits before reporting failure.
pub(crate) const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(6),
];

/// The head every exhausted transient failure carries, followed by the kind
/// token. It matches the token the pending pre-claim deferral
/// (qontinui-runner#1925) uses, `claude_cli_unavailable:<kind>`, so the runner
/// has one word for "the CLI could not be launched".
///
/// For a HEADLESS gate continuation it is the first thing coord receives,
/// because the `spawn_failed` detail is the error's own first line. Phase 3 of
/// the plan teaches coord's `continuation_spawn_retry::classify_spawn_failure`
/// to read that head as a retriable missing CLI. That coord change ships
/// separately, and until it lands coord classifies the head as deterministic,
/// the same as the untagged text it replaces. The agent-loop and scheduler
/// paths add their own prefixes (`spawn failure: `,
/// `spawn scheduled session: `). Neither path is a continuation outcome, so
/// coord's classifier never reads them.
pub(crate) const CLI_UNAVAILABLE_HEAD: &str = "claude_cli_unavailable:";

/// The short kind token for a spawn error that a CLI reinstall in progress can
/// produce, or `None` when the error is not one of those.
///
/// It matches the OS code first and falls back to the portable
/// [`io::ErrorKind`]. That fallback is never used for ENOEXEC or ETXTBSY,
/// which have no stable `ErrorKind` of their own. Windows `193`
/// (`ERROR_BAD_EXE_FORMAT`) is deliberately NOT transient. On this runner it
/// means a direct `CreateProcessW` of the extensionless identity shim, which
/// is a deterministic defect that a retry would only repeat.
pub(crate) fn transient_kind(e: &io::Error) -> Option<&'static str> {
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

/// A spawn that did not produce a child: the last error, and how hard we tried.
#[derive(Debug)]
pub(crate) struct CliSpawnFailure {
    pub(crate) error: io::Error,
    pub(crate) attempts: u32,
    pub(crate) elapsed: Duration,
    /// `Some` when the error was a transient kind and the retry budget was
    /// spent. `None` when the error was not transient, which also means
    /// exactly one attempt was made.
    pub(crate) transient_kind: Option<&'static str>,
}

impl CliSpawnFailure {
    /// The text a caller reports.
    ///
    /// For a non-transient error it keeps the pre-retry shape
    /// (`spawn `<bin>` in <workdir>: <error>`). `<bin>` is now the resolved
    /// path where one resolved. For a spent transient retry the
    /// [`CLI_UNAVAILABLE_HEAD`] token comes FIRST, because coord keeps only a
    /// detail's first 200 characters.
    pub(crate) fn describe(&self, bin: &str, workdir: &str) -> String {
        self.with_head(format!("spawn `{bin}` in {workdir}: {}", self.error))
    }

    /// [`Self::describe`] for a caller whose message never named the program or
    /// the directory: the bare error, with the same head and suffix when the
    /// retry budget was spent.
    pub(crate) fn describe_error(&self) -> String {
        self.with_head(self.error.to_string())
    }

    fn with_head(&self, base: String) -> String {
        match self.transient_kind {
            Some(kind) => format!(
                "{CLI_UNAVAILABLE_HEAD}{kind}: {base} (after {} attempts over {:.1}s; \
                 the CLI may be mid-reinstall by its auto-updater)",
                self.attempts,
                self.elapsed.as_secs_f64()
            ),
            None => base,
        }
    }
}

impl std::fmt::Display for CliSpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

/// Run `attempt` until it succeeds, fails with an error that is not
/// transient, or `delays` is spent. `sleep` is injected so the policy is
/// unit-testable without a clock.
///
/// `is_transient` is the classifier the caller wants. Production passes
/// [`transient_kind`], narrowed by [`spawn_tokio_with_retry`]'s workdir
/// check.
pub(crate) async fn retry_transient_spawn<T, A, C, S, Fut>(
    delays: &[Duration],
    mut attempt: A,
    is_transient: C,
    mut sleep: S,
) -> Result<T, CliSpawnFailure>
where
    A: FnMut() -> io::Result<T>,
    C: Fn(&io::Error) -> Option<&'static str>,
    S: FnMut(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let started = Instant::now();
    let mut attempts: u32 = 0;
    loop {
        attempts += 1;
        let error = match attempt() {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let kind = is_transient(&error);
        let retry_index = (attempts - 1) as usize;
        match (kind, delays.get(retry_index)) {
            (Some(k), Some(&delay)) => {
                warn!(
                    "claude_cli_spawn: transient {k} on attempt {attempts} ({error}); \
                     retrying in {}s",
                    delay.as_secs_f64()
                );
                sleep(delay).await;
            }
            (kind, _) => {
                return Err(CliSpawnFailure {
                    error,
                    attempts,
                    elapsed: started.elapsed(),
                    transient_kind: kind,
                });
            }
        }
    }
}

/// [`transient_kind`], except that a not-found or permission error is NOT
/// transient when the spawn's own working directory is the likely cause.
///
/// A missing `current_dir` also surfaces as `ENOENT` from the spawn. That is a
/// deterministic fault (a reclaimed worktree, for example), not a CLI
/// reinstall. A directory that exists but cannot be entered surfaces as
/// `EACCES` from `chdir`. Retrying either would waste the budget and blame the
/// CLI for it.
pub(crate) fn transient_kind_for_spawn(
    e: &io::Error,
    workdir: Option<&Path>,
) -> Option<&'static str> {
    let kind = transient_kind(e)?;
    if let Some(dir) = workdir {
        if kind == "enoent" && !dir.is_dir() {
            return None;
        }
        // `chdir` needs SEARCH permission on the directory. Looking up `.`
        // inside it needs exactly that, so this tests the same bit.
        if kind == "eacces" && std::fs::metadata(dir.join(".")).is_err() {
            return None;
        }
    }
    Some(kind)
}

/// Spawn `cmd`, retrying a transient CLI launch failure per [`RETRY_DELAYS`].
///
/// A `tokio::process::Command` can be spawned more than once, and each spawn
/// resolves its program again. A bare name goes through the PATH search
/// (`execvp`) again. An absolute path names the symlink or file that the
/// reinstall re-creates at the same place. So each retry re-resolves without
/// the command being rebuilt, and its environment, args and stdio are exactly
/// the caller's.
pub(crate) async fn spawn_tokio_with_retry(
    cmd: &mut tokio::process::Command,
    workdir: Option<&Path>,
) -> Result<tokio::process::Child, CliSpawnFailure> {
    retry_transient_spawn(
        &RETRY_DELAYS,
        || cmd.spawn(),
        |e| transient_kind_for_spawn(e, workdir),
        tokio::time::sleep,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn enoent() -> io::Error {
        io::Error::from(io::ErrorKind::NotFound)
    }

    /// Records each requested back-off instead of sleeping.
    fn recorder(
        log: &RefCell<Vec<Duration>>,
    ) -> impl FnMut(Duration) -> std::future::Ready<()> + '_ {
        move |d| {
            log.borrow_mut().push(d);
            std::future::ready(())
        }
    }

    #[tokio::test]
    async fn enoent_then_success_returns_the_child_on_a_retry() {
        let calls = RefCell::new(0u32);
        let slept = RefCell::new(Vec::new());
        let out = retry_transient_spawn(
            &RETRY_DELAYS,
            || {
                *calls.borrow_mut() += 1;
                if *calls.borrow() < 3 {
                    Err(enoent())
                } else {
                    Ok("child")
                }
            },
            transient_kind,
            recorder(&slept),
        )
        .await
        .expect("third attempt succeeds");
        assert_eq!(out, "child");
        assert_eq!(*calls.borrow(), 3);
        assert_eq!(*slept.borrow(), RETRY_DELAYS[..2].to_vec());
    }

    #[tokio::test]
    async fn persistent_enoent_fails_after_the_bounded_retries() {
        let calls = RefCell::new(0u32);
        let slept = RefCell::new(Vec::new());
        let err = retry_transient_spawn::<(), _, _, _, _>(
            &RETRY_DELAYS,
            || {
                *calls.borrow_mut() += 1;
                Err(enoent())
            },
            transient_kind,
            recorder(&slept),
        )
        .await
        .expect_err("never succeeds");
        let expected_attempts = RETRY_DELAYS.len() as u32 + 1;
        assert_eq!(*calls.borrow(), expected_attempts);
        assert_eq!(err.attempts, expected_attempts);
        assert_eq!(err.transient_kind, Some("enoent"));
        assert_eq!(*slept.borrow(), RETRY_DELAYS.to_vec());
        let total: Duration = slept.borrow().iter().sum();
        assert!(
            total <= Duration::from_secs(10),
            "budget is ~10s, got {total:?}"
        );

        let detail = err.describe("/x/bin/claude", "/w");
        assert!(
            detail.starts_with("claude_cli_unavailable:enoent: spawn `/x/bin/claude` in /w: "),
            "{detail}"
        );
        assert!(detail.contains("after 4 attempts"), "{detail}");
    }

    #[tokio::test]
    async fn a_non_transient_error_is_one_attempt_and_keeps_the_old_text() {
        let calls = RefCell::new(0u32);
        let slept = RefCell::new(Vec::new());
        let err = retry_transient_spawn::<(), _, _, _, _>(
            &RETRY_DELAYS,
            || {
                *calls.borrow_mut() += 1;
                Err(io::Error::other("boom"))
            },
            transient_kind,
            recorder(&slept),
        )
        .await
        .expect_err("fails");
        assert_eq!(*calls.borrow(), 1);
        assert!(slept.borrow().is_empty());
        assert_eq!(err.transient_kind, None);
        assert_eq!(err.describe("claude", "/w"), "spawn `claude` in /w: boom");
    }

    #[test]
    fn transient_kinds_cover_the_measured_reinstall_shapes() {
        assert_eq!(transient_kind(&enoent()), Some("enoent"));
        assert_eq!(
            transient_kind(&io::Error::from(io::ErrorKind::PermissionDenied)),
            Some("eacces")
        );
        assert_eq!(transient_kind(&io::Error::other("x")), None);
        #[cfg(unix)]
        {
            for (code, kind) in [
                (libc::ENOENT, "enoent"),
                (libc::EACCES, "eacces"),
                (libc::ENOEXEC, "enoexec"),
                (libc::ETXTBSY, "etxtbsy"),
            ] {
                assert_eq!(
                    transient_kind(&io::Error::from_raw_os_error(code)),
                    Some(kind),
                    "errno {code}"
                );
            }
            assert_eq!(
                transient_kind(&io::Error::from_raw_os_error(libc::EMFILE)),
                None
            );
            // EPERM maps to ErrorKind::PermissionDenied, but it is not a
            // reinstall shape: an unlisted raw code is never transient.
            assert_eq!(
                transient_kind(&io::Error::from_raw_os_error(libc::EPERM)),
                None
            );
        }
        #[cfg(windows)]
        {
            assert_eq!(
                transient_kind(&io::Error::from_raw_os_error(32)),
                Some("sharing_violation")
            );
            // The shim defect: deterministic, never retried.
            assert_eq!(transient_kind(&io::Error::from_raw_os_error(193)), None);
        }
    }

    #[test]
    fn enoent_from_a_missing_workdir_is_not_transient() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("reclaimed-worktree");
        assert_eq!(transient_kind_for_spawn(&enoent(), Some(&gone)), None);
        assert_eq!(
            transient_kind_for_spawn(&enoent(), Some(tmp.path())),
            Some("enoent")
        );
        assert_eq!(transient_kind_for_spawn(&enoent(), None), Some("enoent"));
    }

    /// End to end over a real `tokio::process::Command`. The program is missing
    /// at the first spawn and appears during the first back-off, the shape of
    /// an npm reinstall window.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_real_spawn_succeeds_once_the_binary_reappears() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("claude");
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.stdout(std::process::Stdio::null());
        let bin_for_sleep = bin.clone();
        let child = retry_transient_spawn(
            // More than one retry: a sibling test thread's fork can briefly
            // hold the freshly written file open (ETXTBSY), itself a transient.
            &[Duration::from_millis(50); 5],
            || cmd.spawn(),
            transient_kind,
            move |d| {
                if !bin_for_sleep.exists() {
                    std::fs::write(&bin_for_sleep, "#!/bin/sh\nexit 0\n").unwrap();
                    std::fs::set_permissions(
                        &bin_for_sleep,
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                }
                std::thread::sleep(d);
                std::future::ready(())
            },
        )
        .await;
        let mut child = child.expect("spawned on the retry");
        assert!(child.wait().await.unwrap().success());
    }
}
