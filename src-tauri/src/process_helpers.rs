//! Helper functions for spawning child processes on Windows without console windows.
//!
//! On Windows, spawning any console program (cmd, powershell, python, node, npx,
//! git, cargo, etc.) from a GUI app creates a visible console window. These helpers
//! set `CREATE_NO_WINDOW` to suppress that. On non-Windows platforms they are
//! transparent passthroughs.

/// Create a `std::process::Command` with `CREATE_NO_WINDOW` on Windows.
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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

// ── Bounded-duration child execution ─────────────────────────────────────────
//
// `std::process::Command::output()` has NO timeout. A child that never exits
// parks the calling thread forever, and when that thread came from tokio's
// blocking pool the parked thread is never returned — enough of those and
// `spawn_blocking` stops scheduling, which is stage 1 of the 2026-08-23 runner
// wedge (a hung `git` behind an index.lock / credential prompt / unreachable
// remote held blocking threads until the pool was exhausted).
//
// [`run_with_timeout`] is the replacement: it always returns within the budget,
// and it KILLS + reaps the child on expiry so a hung subprocess cannot outlive
// the call.

/// Outcome of [`run_with_timeout`].
#[derive(Debug)]
pub enum TimedOutput {
    /// The child exited on its own inside the budget.
    Completed(std::process::Output),
    /// The child overran the budget; it was killed.
    TimedOut {
        /// The killed child's OS pid — for the caller's WARN line.
        pid: u32,
        /// Whether the kill was followed by a successful `wait()` (i.e. the
        /// child was reaped and left no zombie). Reported rather than assumed
        /// so a test can assert it.
        reaped: bool,
    },
}

/// Run `cmd` to completion, but never for longer than `timeout`.
///
/// Semantics:
/// - stdin is `/dev/null` so a child can never block waiting for input (this is
///   also what stops a credential prompt from hanging forever).
/// - stdout/stderr are piped and drained by two short-lived reader threads, so
///   a chatty child cannot deadlock on a full pipe buffer.
/// - On expiry the child is killed and reaped, and `TimedOut` is returned. The
///   reader threads are NOT joined on that path: joining could re-introduce the
///   very hang we are escaping (a killed child's grandchildren can keep the
///   pipe write-ends open). They exit on their own when the pipes close.
///
/// Returns `Err` only when the child could not be spawned or `try_wait` failed.
pub fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<TimedOutput> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let pid = child.id();

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    // Back off from a tight poll to a coarse one: a fast `git rev-parse` still
    // returns in ~2ms, while a long-running child costs at most 20 wakeups/s.
    let mut poll = Duration::from_millis(2);
    let max_poll = Duration::from_millis(50);

    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = stdout_reader.join().unwrap_or_default();
                let stderr = stderr_reader.join().unwrap_or_default();
                return Ok(TimedOutput::Completed(std::process::Output {
                    status,
                    stdout,
                    stderr,
                }));
            }
            None => {
                let now = Instant::now();
                if now >= deadline {
                    let _ = child.kill();
                    let reaped = child.wait().is_ok();
                    return Ok(TimedOutput::TimedOut { pid, reaped });
                }
                std::thread::sleep(poll.min(deadline - now));
                poll = (poll * 2).min(max_poll);
            }
        }
    }
}

/// Drop-in bounded replacement for `Command::output()`.
///
/// Returns exactly what [`std::process::Command::output`] returns, so a call
/// site converts by wrapping the built command and leaving every downstream
/// arm (`.ok()?`, `.and_then(|o| …)`, `match { Ok(o) if o.status.success() … }`)
/// untouched — EXCEPT that a child which overruns `timeout` is killed, reaped,
/// and surfaced as `Err(ErrorKind::TimedOut)` instead of parking the calling
/// thread forever.
///
/// Use this where the caller already has bespoke handling for `Output` and a
/// timeout is honestly just one more way to fail; use [`run_probe`] where the
/// caller only wants stdout-or-degrade.
pub fn output_with_timeout(
    cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let label = format!("{:?}", cmd);
    match run_with_timeout(cmd, timeout) {
        Ok(TimedOutput::Completed(o)) => Ok(o),
        Ok(TimedOutput::TimedOut { pid, reaped }) => {
            tracing::warn!(
                child_pid = pid,
                reaped,
                timeout_secs = timeout.as_secs(),
                "subprocess timed out and was killed: {label}"
            );
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "{label} exceeded its {}s budget and was killed (pid={pid}, reaped={reaped})",
                    timeout.as_secs()
                ),
            ))
        }
        Err(e) => Err(e),
    }
}

/// Why a [`run_probe`] call degraded — carried so a caller (or a test) can
/// tell a real timeout apart from an ordinary non-zero exit.
#[derive(Debug, PartialEq, Eq)]
pub enum DegradeReason {
    /// The child exited non-zero inside the budget.
    Status,
    /// The child could not be spawned at all.
    SpawnError,
    /// The child overran the budget and was killed. `reaped` says whether the
    /// follow-up `wait()` succeeded, so a leaked zombie is observable.
    TimedOut { pid: u32, reaped: bool },
}

/// Outcome of one bounded external probe — see [`run_probe`].
#[derive(Debug)]
pub enum ProbeOutcome {
    /// The child exited 0 inside the budget; carries its raw stdout.
    Captured(Vec<u8>),
    /// Anything else. Callers of this shape all degrade identically (empty
    /// result / `None` / `false`); the reason is for the log and for tests.
    Degraded(DegradeReason),
}

/// Run one external probe under a hard budget, mapping every failure mode onto
/// [`ProbeOutcome::Degraded`] and emitting a WARN naming `label`.
///
/// This is the ergonomic form of [`run_with_timeout`] for the overwhelmingly
/// common "shell out, read stdout, degrade on anything else" shape — the shape
/// that produced the 2026-08-23 and 2026-08-30 runner wedges when written as a
/// bare `Command::output()`. Prefer it over hand-rolling the four match arms:
/// a caller that forgets the `TimedOut` arm is exactly how the defect recurs.
///
/// `label` is used verbatim in the WARN, so pass something that identifies the
/// module and the operation (e.g. `"process_tree: PowerShell snapshot"`).
pub fn run_probe(
    cmd: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> ProbeOutcome {
    match run_with_timeout(cmd, timeout) {
        Ok(TimedOutput::Completed(o)) if o.status.success() => ProbeOutcome::Captured(o.stdout),
        Ok(TimedOutput::Completed(o)) => {
            tracing::warn!(
                "{label} failed (status={:?}, stderr={})",
                o.status,
                String::from_utf8_lossy(&o.stderr)
            );
            ProbeOutcome::Degraded(DegradeReason::Status)
        }
        Ok(TimedOutput::TimedOut { pid, reaped }) => {
            // WARN, not debug: a silent timeout just relocates the mystery.
            // The killed pid + the budget are what make the next incident
            // diagnosable from the log alone.
            tracing::warn!(
                child_pid = pid,
                reaped,
                timeout_secs = timeout.as_secs(),
                "{label} timed out and was killed — degrading this pass"
            );
            ProbeOutcome::Degraded(DegradeReason::TimedOut { pid, reaped })
        }
        Err(e) => {
            tracing::warn!("{label} spawn error: {e}");
            ProbeOutcome::Degraded(DegradeReason::SpawnError)
        }
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A command that blocks for far longer than any budget we hand it.
    fn sleeper() -> std::process::Command {
        #[cfg(target_os = "windows")]
        {
            let mut c = no_window("cmd.exe");
            // `ping -n 60` ≈ 59s of blocking with no console window.
            c.args(["/C", "ping -n 60 127.0.0.1"]);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = no_window("sh");
            c.args(["-c", "sleep 60"]);
            c
        }
    }

    /// Item 1 core assertion: a child that never returns must NOT hold the
    /// calling thread. Without the timeout this test hangs for ~59s and the
    /// elapsed assertion fails.
    #[test]
    fn a_blocking_child_returns_within_the_budget_and_is_reaped() {
        let budget = Duration::from_millis(400);
        let started = Instant::now();
        let out = run_with_timeout(sleeper(), budget).expect("spawn");
        let elapsed = started.elapsed();

        match out {
            TimedOutput::TimedOut { pid, reaped } => {
                assert!(pid > 0, "a killed child must report its pid");
                assert!(reaped, "the timed-out child must be waited on, not leaked");
            }
            TimedOutput::Completed(o) => {
                panic!("expected a timeout, got exit {:?}", o.status);
            }
        }
        assert!(
            elapsed < budget * 8,
            "run_with_timeout blocked for {elapsed:?}, budget was {budget:?}"
        );
    }

    // ── Bounded-probe / blocking-pool regression tests (2026-08-30 Phase 2) ──
    //
    // `run_probe` is the seam every `#[cfg(windows)]`-gated WMI / netstat /
    // lsof call site now routes through, and it lives here — in the LIB — on
    // purpose: the leaking call sites are Windows-only, so without an
    // un-gated seam the bounded-execution behaviour could never be exercised
    // by CI on any other platform. Each of these fails if the routing is
    // reverted to a bare `Command::output()`.

    /// A hung probe must give its thread back inside the budget, kill the
    /// child, and reap it.
    #[test]
    fn run_probe_degrades_within_budget_and_reaps() {
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let outcome = run_probe(sleeper(), budget, "test: hung probe");
        let elapsed = started.elapsed();

        match outcome {
            ProbeOutcome::Degraded(DegradeReason::TimedOut { pid, reaped }) => {
                assert!(pid > 0, "the killed child must report its pid");
                assert!(
                    reaped,
                    "a timed-out child must be reaped, not left a zombie"
                );
            }
            other => panic!("expected a TimedOut degrade, got {other:?}"),
        }
        assert!(
            elapsed < budget * 8,
            "run_probe held its thread for {elapsed:?} against a {budget:?} budget"
        );
    }

    /// A non-zero exit is a `Status` degrade, NOT a timeout — the two must
    /// stay distinguishable or the WARN and this test both lose their meaning.
    #[test]
    fn run_probe_distinguishes_a_failing_child_from_a_hung_one() {
        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = no_window("cmd.exe");
            c.args(["/C", "exit 3"]);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = no_window("sh");
            c.args(["-c", "exit 3"]);
            c
        };
        assert!(matches!(
            run_probe(cmd, Duration::from_secs(20), "test: failing probe"),
            ProbeOutcome::Degraded(DegradeReason::Status)
        ));
    }

    /// `output_with_timeout` must surface a hang as `ErrorKind::TimedOut`, so
    /// every call site's existing `Err` arm covers it — and must NOT report it
    /// as a completed child with a non-zero status.
    #[test]
    fn output_with_timeout_reports_a_hang_as_an_io_timeout() {
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let err = output_with_timeout(sleeper(), budget)
            .expect_err("a hung child must be Err, never Ok(Output)");
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        let msg = err.to_string();
        assert!(
            msg.contains("pid=") && msg.contains("reaped=true"),
            "the error must name the killed pid and prove it was reaped; got {msg:?}"
        );
        assert!(started.elapsed() < budget * 8);
    }

    /// **The wedge itself, in miniature.**
    ///
    /// Before the fix, a degraded WMI provider meant every periodic caller's
    /// probe permanently consumed one blocking-pool thread; because the
    /// callers re-fire on independent timers the stuck threads accumulated
    /// until tokio's 512-thread default was exhausted and `spawn_blocking`
    /// stopped scheduling anything at all.
    ///
    /// Here: a pool capped at `K`, `N > K` concurrent probes that ALL hang.
    /// The assertions are (a) every probe returns rather than parking forever,
    /// and (b) — the one that actually separates fixed from broken — a
    /// SUBSEQUENT `spawn_blocking` is still scheduled promptly, i.e. the
    /// threads went back to the pool. With an unbounded `.output()` the
    /// follow-up never runs and the outer `timeout` FAILS the test instead of
    /// hanging CI for the sleeper's full minute.
    #[test]
    fn hung_probes_do_not_permanently_consume_the_blocking_pool() {
        const K: usize = 4; // blocking-pool cap
        const N: usize = 16; // concurrent hanging probes, comfortably > K
        let budget = Duration::from_millis(300);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(K)
            .enable_all()
            .build()
            .expect("build runtime");

        rt.block_on(async move {
            let timed_out = tokio::time::timeout(Duration::from_secs(30), async move {
                let mut handles = Vec::with_capacity(N);
                for _ in 0..N {
                    handles.push(spawn_blocking_tracked(move || {
                        run_probe(sleeper(), budget, "test: pool-pressure probe")
                    }));
                }
                let mut n = 0usize;
                for h in handles {
                    match h.await.expect("blocking task must not panic") {
                        ProbeOutcome::Degraded(DegradeReason::TimedOut { reaped, .. }) => {
                            assert!(reaped, "every timed-out child must be reaped");
                            n += 1;
                        }
                        other => panic!("expected every probe to time out, got {other:?}"),
                    }
                }
                n
            })
            .await
            .expect("the hanging probes never returned — the blocking pool is wedged");

            assert_eq!(timed_out, N, "every probe must report a bounded timeout");

            // The load-bearing half: the pool must be usable again. Had the
            // threads leaked, this never gets a slot.
            let reused =
                tokio::time::timeout(Duration::from_secs(10), spawn_blocking_tracked(|| 7u32))
                    .await
                    .expect("the blocking pool was still saturated after the probes returned")
                    .expect("blocking task must not panic");
            assert_eq!(reused, 7);
        });
    }

    /// The fast path must still deliver the child's real output.
    #[test]
    fn a_fast_child_completes_normally_with_its_stdout() {
        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = no_window("cmd.exe");
            c.args(["/C", "echo hello-from-child"]);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = no_window("sh");
            c.args(["-c", "echo hello-from-child"]);
            c
        };

        match run_with_timeout(cmd, Duration::from_secs(20)).expect("spawn") {
            TimedOutput::Completed(o) => {
                assert!(o.status.success());
                let s = String::from_utf8_lossy(&o.stdout);
                assert!(s.contains("hello-from-child"), "got stdout {s:?}");
            }
            TimedOutput::TimedOut { .. } => panic!("a trivial echo must not time out"),
        }
    }
}
