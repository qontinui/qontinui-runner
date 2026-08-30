//! Helper functions for spawning child processes on Windows without console windows.
//!
//! On Windows, spawning any console program (cmd, powershell, python, node, npx,
//! git, cargo, etc.) from a GUI app creates a visible console window. These helpers
//! set `CREATE_NO_WINDOW` to suppress that. On non-Windows platforms they are
//! transparent passthroughs.

/// Create a `std::process::Command` with `CREATE_NO_WINDOW` on Windows.
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

// ── Regression guard: no un-suppressed console spawns ─────────────────────────
//
// The runner is a GUI-subsystem process in release builds
// (`main.rs`: `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]`),
// so it owns no console. On Windows every console-subsystem child spawned
// WITHOUT `CREATE_NO_WINDOW` therefore allocates a fresh console of its own —
// a window that flashes open and shut for the life of the command.
//
// Commit 2318026ae fixed 89 such sites at once. It could not stop the NEXT one:
// `session_pr_reconciler` (written a week later) re-introduced the highest-rate
// spawn site in the runner and shipped in v1.0.10, and `fleet.rs`'s auto-fresh
// engine did the same. Nobody caught either, because a DEBUG build is
// console-subsystem — its children inherit the runner's own console and nothing
// flashes. The people who would notice run debug builds; the people who run
// release builds are users.
//
// So the rule needs a test rather than a reviewer.

#[cfg(test)]
mod console_window_guard {
    use std::path::{Path, PathBuf};

    /// How far below a `Command::new(` line a suppression (`creation_flags`,
    /// or a `no_window(&mut cmd)`-style call) still counts as covering that
    /// spawn. Generous on purpose: the builders in this crate set the flag
    /// after a run of `.arg()` calls.
    const FLAG_WINDOW: usize = 25;

    /// Files the rule does not apply to.
    ///
    /// `src/bin/` holds standalone CONSOLE-subsystem binaries (`qontinui_cli`,
    /// `qontinui_shim`). They already own a console, their children inherit it,
    /// and nothing flashes — the defect is specific to the GUI binary.
    fn exempt(rel: &Path) -> bool {
        let s = rel.to_string_lossy().replace('\\', "/");
        s.starts_with("bin/") || s == "process_helpers.rs"
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// Every `Command::new(` in production code must either be suppressed
    /// (`process_helpers::{no_window, tokio_no_window, …}`, or an inline
    /// `creation_flags`) or carry a `console-ok:` marker saying why a console
    /// there is fine — a non-Windows-only arm, a deliberately visible terminal,
    /// or a builder that is never spawned.
    #[test]
    fn every_spawn_site_is_suppressed_or_marked_console_ok() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&root, &mut files);
        assert!(
            files.len() > 100,
            "walked only {} files under {} — the guard scanned nothing",
            files.len(),
            root.display()
        );

        let mut violations: Vec<String> = Vec::new();

        for file in files {
            let rel = file.strip_prefix(&root).unwrap_or(&file).to_path_buf();
            if exempt(&rel) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&file) else {
                continue;
            };
            // Production text only: a spawn written inside a test module is
            // not shipped, so it cannot flash anything on a user's machine.
            let prod = src
                .split_once("\n#[cfg(test)]")
                .map(|(before, _)| before)
                .unwrap_or(&src);
            let lines: Vec<&str> = prod.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                if !line.contains("Command::new(") {
                    continue;
                }
                let trimmed = line.trim_start();
                // Prose, not code.
                if trimmed.starts_with("//") {
                    continue;
                }
                // Suppressed inline, a few lines down — either the raw flag
                // or a helper that sets it on an already-built command
                // (`pm_detect::no_window(&mut cmd)`).
                let end = (i + FLAG_WINDOW).min(lines.len());
                if lines[i..end]
                    .iter()
                    .any(|l| l.contains("creation_flags") || l.contains("no_window("))
                {
                    continue;
                }
                // Explicitly marked, on the line or just above it.
                let start = i.saturating_sub(3);
                if lines[start..=i].iter().any(|l| l.contains("console-ok:")) {
                    continue;
                }
                violations.push(format!(
                    "{}:{}: {}",
                    rel.to_string_lossy().replace('\\', "/"),
                    i + 1,
                    trimmed
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "un-suppressed console spawn site(s) — on Windows each of these pops a \
             console window on every call in a release build.\n\n{}\n\nFix: build the \
             command with `crate::process_helpers::no_window(..)` / `tokio_no_window(..)` \
             instead of `Command::new(..)`. If a console there is CORRECT (a \
             non-Windows-only arm, a deliberately visible terminal, a builder that is \
             never spawned), say so with a `// console-ok: <reason>` comment on the line \
             or just above it.",
            violations.join("\n")
        );
    }
}
