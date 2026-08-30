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
/// ## Two halves, because the two platforms address a tree differently
///
/// - Windows: the job object is created and the child assigned to it AFTER the
///   spawn — [`ChildTreeGuard::attach`]. [`ChildTreeGuard::arm`] is a no-op.
/// - Unix: a process group has to be established BETWEEN fork and exec, so
///   [`ChildTreeGuard::arm`] must be called on the `Command` before spawning
///   and [`ChildTreeGuard::attach_armed`] afterwards. `attach` on its own stays
///   a deliberate no-op on Unix: killing a group we did not create would signal
///   our OWN process group.
///
/// [`ChildTreeGuard::disarm`] releases the tree without killing it — used on
/// the success path of [`run_with_timeout`], see the comment there.
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
    /// Pre-spawn half of the reaper. No-op on Windows — the job object is
    /// created after the spawn, in [`Self::attach`].
    pub fn arm(_cmd: &mut std::process::Command) {}

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

    /// Post-spawn form for a command that went through [`Self::arm`]. On
    /// Windows that is exactly [`Self::attach`].
    pub fn attach_armed(child: &std::process::Child) -> Self {
        Self::attach(child)
    }

    /// Release the tree WITHOUT killing it.
    ///
    /// Clears `KILL_ON_JOB_CLOSE` first, so closing the last handle simply
    /// dissolves the job. If clearing fails we still close — a kill is the
    /// safer degrade than leaking a job handle for the process lifetime.
    pub fn disarm(mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        };
        if let Some(job) = self.0.take() {
            unsafe {
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = 0;
                let cleared = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(info).cast(),
                    u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                        .unwrap_or(0),
                );
                if cleared == 0 {
                    tracing::debug!(
                        "ChildTreeGuard::disarm could not clear KILL_ON_JOB_CLOSE; \
                         the tree will be terminated instead of released"
                    );
                }
                CloseHandle(job);
            }
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

/// Non-Windows form — see the Windows one for what this is for.
///
/// Carries the child's process-group id, and ONLY when the command was
/// [`ChildTreeGuard::arm`]ed so that group is one we created.
#[cfg(not(windows))]
pub struct ChildTreeGuard(Option<i32>);

#[cfg(not(windows))]
impl ChildTreeGuard {
    /// Pre-spawn: make the child the leader of its own process group
    /// (`setpgid(0, 0)` between fork and exec), so the child and every
    /// descendant that does not deliberately leave the group are addressable
    /// by a single `killpg`.
    ///
    /// This is the Unix answer to the Windows job object, and it MUST happen
    /// before the spawn — after it, the child may already have exec'd.
    pub fn arm(cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    /// No-op: without [`Self::arm`] the child shares OUR process group, and
    /// `killpg` on it would signal this process. Nothing is attached and
    /// nothing is reaped — exactly the previous behaviour.
    pub fn attach(_child: &std::process::Child) -> Self {
        Self(None)
    }

    /// Post-spawn form for an [`Self::arm`]ed command: the child *is* its
    /// group leader, so its pid doubles as the pgid.
    pub fn attach_armed(child: &std::process::Child) -> Self {
        Self(i32::try_from(child.id()).ok().filter(|pgid| *pgid > 1))
    }

    /// Release the group WITHOUT killing it.
    pub fn disarm(mut self) {
        self.0 = None;
    }
}

#[cfg(not(windows))]
impl Drop for ChildTreeGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.0.take() {
            // SIGKILL the whole group. Best-effort: ESRCH just means the group
            // is already gone, which is the outcome we wanted anyway.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
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
// There are TWO doors here, and which one a call site uses is a statement about
// the CHILD'S LIFETIME that the helper cannot infer:
//
//   * [`run_with_timeout`] & friends — the child's life is bounded by the call.
//     stdout/stderr are piped and captured, and on expiry the whole process TREE
//     is killed and reaped. Every probe belongs here.
//   * [`start_detached`] — the caller declares the child MAY OUTLIVE the call
//     (`fleet`'s `start_command`, whose entire purpose is to leave a server
//     running). Nothing is captured and nothing is ever killed.
//
// Three resources, three independent bounds, none of them a function of how
// long a surviving descendant lives:
//
//   * the calling thread   -> `timeout + COMPLETED_DRAIN_GRACE`
//   * the two reader threads -> abandoned at the return, `READER_ABANDON_POLL`
//   * the captured bytes   -> `2 * MAX_CAPTURED_BYTES`
//
// Making the reader threads independent of pipe EOF is what lets the capture
// door leave a descendant alive without leaking threads or memory. It is ALSO
// why the capture door must never be pointed at a child meant to survive: an
// abandoned reader drops the pipe READ end, and the survivor holding the write
// end is then killed by SIGPIPE (`std::process::Command` resets SIGPIPE to
// `SIG_DFL` in the child). That is what [`start_detached`] exists to avoid, by
// never creating the pipe in the first place.

/// How long the success path will wait for the pipe readers to see EOF after
/// the child has already exited.
///
/// Bounded rather than a `join()`, because EOF is NOT guaranteed to arrive: a
/// grandchild that outlived the child still holds duplicates of both write
/// ends (a `cmd /c` shim, a corepack/volta background update check, a git
/// credential helper). A join there blocks forever *on the success path*,
/// inside a blocking-pool thread — the very leak this module exists to close.
const COMPLETED_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the timeout path waits for the readers after the tree has been
/// killed. Purely thread hygiene — the output is discarded on that path — and
/// bounded for the same reason as [`COMPLETED_DRAIN_GRACE`].
const KILLED_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_millis(500);

/// Hard cap on how many bytes ONE pipe will buffer on the caller's behalf.
///
/// Without a cap the reader's `Vec` grows for as long as SOMETHING is writing
/// to the pipe — and on the success path that "something" outlives the call
/// (see [`run_with_timeout`]'s doc). A descendant the child left behind on our
/// inherited stdout — a corepack/volta background update check, a git
/// credential or askpass helper, a `cmd /c` shim's grandchild — would then grow
/// a buffer, in a process measured in weeks, that no one will ever read. That
/// is the same unbounded-resource defect this module exists to close, relocated
/// from threads to memory.
///
/// (A DELIBERATELY long-lived child no longer reaches this door at all — it
/// goes through [`start_detached`], which pipes nothing. The bound still has to
/// hold, because an accidental survivor is not a thing the caller can predict.)
///
/// Past the cap the reader keeps DRAINING and DISCARDS what it reads. It does
/// not stop reading: a reader that stopped would let the pipe buffer fill, and
/// a child blocked writing to a full pipe never exits — turning "produced a lot
/// of output" into "timed out and was killed". Draining costs nothing (the
/// thread is bounded by [`READER_ABANDON_POLL`] anyway) and keeps the child's
/// exit status honest.
///
/// Precedent for the value's shape: `wrappers/registry.rs`'s
/// `MAX_MANIFEST_STDOUT_BYTES` (256 KiB). This one is larger because the
/// callers here include operator-configured BUILD commands, whose legitimate
/// logs run to megabytes.
pub const MAX_CAPTURED_BYTES: usize = 4 * 1024 * 1024;

/// How long a reader blocks in ONE wait before re-checking whether the call
/// that owns its buffer has already returned.
///
/// This is the knob that makes a reader thread's lifetime a property of the
/// CALL rather than of the pipe: EOF may never arrive, but abandonment always
/// does, at the latest one poll after `run_with_timeout` returns.
const READER_ABANDON_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Number of pipe-reader threads currently alive across the whole process.
///
/// Every reader increments on spawn and decrements when it returns, so this is
/// a direct gauge of "reader threads this module is holding". Exposed via
/// [`live_pipe_readers`] so a regression test can assert on evidence rather
/// than on timing.
static LIVE_PIPE_READERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many pipe-reader threads spawned by this module are alive right now.
pub fn live_pipe_readers() -> usize {
    LIVE_PIPE_READERS.load(std::sync::atomic::Ordering::Acquire)
}

/// Outcome of waiting a bounded time for a pipe to become readable.
enum PipeReady {
    /// A `read` will not block — there are bytes, or the write ends closed, or
    /// the wait itself failed and the `read` should surface why.
    Ready,
    /// Nothing happened inside the wait. The caller gets to re-check whether it
    /// has been abandoned before blocking again.
    TimedOut,
}

/// A pipe end this module can wait on WITH A TIMEOUT.
///
/// The whole point of this trait is that a plain `Read::read` on a pipe is an
/// UNINTERRUPTIBLE block: nothing the parent does — not dropping its handles,
/// not setting a flag — wakes a thread parked in that syscall, and closing the
/// fd from another thread does not either. A reader that can only block in
/// `read` therefore lives exactly as long as the write ends stay open, which on
/// the success path is the descendant's lifetime, not the call's.
///
/// Waiting with a timeout first is what converts that into a bounded thread:
/// the reader wakes at least every [`READER_ABANDON_POLL`], notices it has been
/// abandoned, and exits.
trait WaitablePipe: std::io::Read + Send + 'static {
    fn wait_readable(&self, timeout: std::time::Duration) -> PipeReady;
}

#[cfg(not(windows))]
fn wait_readable_raw(fd: std::os::fd::RawFd, timeout: std::time::Duration) -> PipeReady {
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialised `pollfd` describing a live fd we own.
    let rc = unsafe { libc::poll(std::ptr::addr_of_mut!(pfd), 1, ms) };
    if rc > 0 {
        // Bytes, POLLHUP or POLLERR — in all three the following `read` returns
        // the data, the EOF or the real error, so there is nothing to decide
        // here.
        return PipeReady::Ready;
    }
    if rc == 0 {
        return PipeReady::TimedOut;
    }
    // rc < 0. **EINTR must NOT become `Ready`.** `poll(2)` is explicitly not
    // restarted by `SA_RESTART` (signal(7)), and this process installs exactly
    // such a handler for SIGCHLD as soon as any `tokio::process::Child` is
    // spawned — so a reader can be woken with `errno == EINTR` and no data.
    // Reporting `Ready` there sends it straight into the UNINTERRUPTIBLE
    // `read` this whole mechanism exists to avoid, and it then lives as long as
    // the pipe rather than as long as the call: the exact leak this module was
    // written to close. Report `TimedOut` instead — the caller re-checks the
    // abandoned flag and polls again, which is both correct and bounded.
    //
    // Any OTHER error (EBADF, EINVAL, ENOMEM, a bad fd handed to us) is a wait
    // that will never work. `Ready` is the right answer for those: the `read`
    // that follows fails too, and the reader breaks out of its loop and exits.
    // Spinning on `TimedOut` there would burn a core until abandonment.
    match std::io::Error::last_os_error().raw_os_error() {
        Some(e) if e == libc::EINTR => PipeReady::TimedOut,
        _ => PipeReady::Ready,
    }
}

#[cfg(not(windows))]
impl<T: std::io::Read + std::os::fd::AsRawFd + Send + 'static> WaitablePipe for T {
    fn wait_readable(&self, timeout: std::time::Duration) -> PipeReady {
        wait_readable_raw(self.as_raw_fd(), timeout)
    }
}

// Declared here rather than pulled from `windows-sys` because `PeekNamedPipe`
// lives behind the `Win32_System_Pipes` feature, which this crate does not
// enable — and a Cargo.toml edit is a far wider blast radius than one stable
// kernel32 export whose signature has not changed since NT 3.1.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn PeekNamedPipe(
        hNamedPipe: windows_sys::Win32::Foundation::HANDLE,
        lpBuffer: *mut core::ffi::c_void,
        nBufferSize: u32,
        lpBytesRead: *mut u32,
        lpTotalBytesAvail: *mut u32,
        lpBytesLeftThisMessage: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
fn wait_readable_raw(
    handle: windows_sys::Win32::Foundation::HANDLE,
    timeout: std::time::Duration,
) -> PipeReady {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + timeout;
    // Windows has no `poll` for an anonymous pipe, so readability is sampled.
    // Back the sampling off rather than running a flat 100 Hz for the life of
    // the call: a 30-minute build with a quiet stream was ~360k wakeups across
    // its two pipes. The backoff costs nothing on a CHATTY stream, because the
    // very first `PeekNamedPipe` already reports bytes and returns before any
    // sleep — and it resets on every call, i.e. after every read.
    let mut poll = Duration::from_millis(1);
    let max_poll = Duration::from_millis(50);
    loop {
        let mut avail: u32 = 0;
        // SAFETY: `handle` is the live read end this reader owns; every other
        // pointer is null, which `PeekNamedPipe` documents as "not wanted".
        let ok = unsafe {
            PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(avail),
                std::ptr::null_mut(),
            )
        };
        // A FALSE return is ERROR_BROKEN_PIPE in practice (the write ends
        // closed) — report Ready and let the `read` turn it into the Ok(0) or
        // the error it really is, rather than guessing here.
        if ok == 0 || avail > 0 {
            return PipeReady::Ready;
        }
        let now = Instant::now();
        if now >= deadline {
            return PipeReady::TimedOut;
        }
        std::thread::sleep(poll.min(deadline - now));
        poll = (poll * 2).min(max_poll);
    }
}

#[cfg(windows)]
impl<T: std::io::Read + std::os::windows::io::AsRawHandle + Send + 'static> WaitablePipe for T {
    fn wait_readable(&self, timeout: std::time::Duration) -> PipeReady {
        wait_readable_raw(self.as_raw_handle() as _, timeout)
    }
}

/// One detached pipe reader that publishes into a shared buffer.
///
/// Publishing (rather than returning from a `JoinHandle`) is what makes a
/// BOUNDED wait possible: whatever the child managed to write is readable even
/// when the reader itself never gets EOF.
///
/// Three bounds, because "the caller returns promptly" is not on its own a
/// statement about what the reader keeps consuming afterwards:
///
/// - **Memory** — the buffer never exceeds [`MAX_CAPTURED_BYTES`]; past that
///   the reader drains and discards, and sets [`Self::was_truncated`].
/// - **Thread** — dropping the handle sets `abandoned`, and the reader checks
///   it at least every [`READER_ABANDON_POLL`], so a reader outlives its call
///   by at most one poll no matter what still holds the write ends.
/// - **Honesty** — a buffer that is missing bytes says so, so the caller can
///   refuse to present a partial read as a complete one.
struct PipeDrain {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    truncated: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by [`Drop`], never by hand. The reader owns clones of the two Arcs
    /// above, so it cannot notice the handle going away on its own — and an
    /// abandonment that a future return path could FORGET to signal is the
    /// leak we are closing, so it is wired to scope exit rather than to a call.
    abandoned: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// `true` when the reader THREAD could not be created at all, so this
    /// stream was never read. Not shared: it is decided before the reader could
    /// exist. Surfaced as [`Truncation::ReaderUnavailable`] rather than folded
    /// into "empty output", which would read as *the child printed nothing*.
    reader_unavailable: bool,
}

impl PipeDrain {
    fn spawn<R: WaitablePipe>(pipe: Option<R>) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        let buf = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let truncated = Arc::new(AtomicBool::new(false));
        let abandoned = Arc::new(AtomicBool::new(false));
        let mut reader_unavailable = false;

        match pipe {
            None => {
                // Nothing to read: already at EOF by definition.
                done.store(true, Ordering::Release);
            }
            Some(mut pipe) => {
                let buf_t = Arc::clone(&buf);
                let done_t = Arc::clone(&done);
                let truncated_t = Arc::clone(&truncated);
                let abandoned_t = Arc::clone(&abandoned);
                LIVE_PIPE_READERS.fetch_add(1, Ordering::AcqRel);
                let spawned = std::thread::Builder::new()
                    .name("pipe-drain".to_string())
                    .spawn(move || {
                        use std::io::Read;
                        let mut chunk = [0u8; 8192];
                        let mut captured = 0usize;
                        loop {
                            if abandoned_t.load(Ordering::Acquire) {
                                // The call that wanted this output has returned.
                                // Whatever arrives now would be written into a
                                // buffer nobody will ever read.
                                break;
                            }
                            if matches!(
                                pipe.wait_readable(READER_ABANDON_POLL),
                                PipeReady::TimedOut
                            ) {
                                continue;
                            }
                            match pipe.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(n) => {
                                    let room = MAX_CAPTURED_BYTES.saturating_sub(captured);
                                    if room < n {
                                        truncated_t.store(true, Ordering::Release);
                                    }
                                    let take = n.min(room);
                                    if take > 0 {
                                        if let Ok(mut g) = buf_t.lock() {
                                            g.extend_from_slice(&chunk[..take]);
                                        }
                                        captured += take;
                                    }
                                    // Bytes past `take` are deliberately dropped on
                                    // the floor — see [`MAX_CAPTURED_BYTES`] for why
                                    // we keep reading them rather than stopping.
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                Err(_) => break,
                            }
                        }
                        done_t.store(true, Ordering::Release);
                        LIVE_PIPE_READERS.fetch_sub(1, Ordering::AcqRel);
                    });
                // `std::thread::spawn` PANICS when the OS refuses a thread —
                // and `EAGAIN` under thread exhaustion is precisely the
                // condition this module exists to address, so the panic would
                // fire exactly when things are already bad, propagate out of a
                // `spawn_blocking` body, and leave the gauge above permanently
                // inflated. `Builder::spawn` hands back the error instead.
                if let Err(e) = spawned {
                    LIVE_PIPE_READERS.fetch_sub(1, Ordering::AcqRel);
                    // No reader ⇒ nothing will ever be read from this pipe. Say
                    // so: `done` unblocks `wait_for_drain` (there is nothing to
                    // wait for) and `reader_unavailable` makes the emptiness
                    // report as INCOMPLETE rather than as "the child was quiet".
                    done.store(true, Ordering::Release);
                    reader_unavailable = true;
                    tracing::warn!(
                        error = %e,
                        "could not start a pipe-reader thread; this child's stdout/stderr \
                         will NOT be captured. The pipe read end is closed immediately, so \
                         the child sees EPIPE rather than blocking on a full pipe."
                    );
                }
            }
        }

        Self {
            buf,
            done,
            truncated,
            abandoned,
            reader_unavailable,
        }
    }

    fn is_done(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Acquire)
    }

    fn was_truncated(&self) -> bool {
        self.truncated.load(std::sync::atomic::Ordering::Acquire)
    }

    fn reader_unavailable(&self) -> bool {
        self.reader_unavailable
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

impl Drop for PipeDrain {
    fn drop(&mut self) {
        // The only abandonment signal, and it fires on EVERY exit from
        // `run_with_timeout` — including a `?` and a panic unwind — because it
        // is scope exit rather than a statement someone has to remember.
        self.abandoned
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Wait — on a HARD clock — for both readers to reach EOF. Returns whether
/// they did. Never a `join()`: see [`COMPLETED_DRAIN_GRACE`].
fn wait_for_drain(a: &PipeDrain, b: &PipeDrain, grace: std::time::Duration) -> bool {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + grace;
    let mut poll = Duration::from_millis(1);
    loop {
        if a.is_done() && b.is_done() {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(poll.min(deadline - now));
        poll = (poll * 2).min(Duration::from_millis(25));
    }
}

/// Outcome of [`run_with_timeout`].
#[derive(Debug)]
pub enum TimedOutput {
    /// The child exited on its own inside the budget.
    ///
    /// **Says nothing about whether the captured output is COMPLETE** — that is
    /// [`TimedRun::truncation`], which this type cannot carry because
    /// [`std::process::Output`] has no field for it. A caller that turns this
    /// into a definite answer about the child's output (rather than about its
    /// exit status) must go through [`run_with_timeout_detailed`] or
    /// [`run_probe`], both of which surface truncation explicitly.
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

/// Why a captured stream is INCOMPLETE.
///
/// Exists because "we read only part of it" is a THIRD answer, distinct from
/// both "here is the output" and "the probe failed". Folding it into the first
/// is how a `git status --porcelain` that was cut short reads as *clean* — a
/// definite verdict derived from bytes we never saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    /// The child exited, but a surviving descendant still holds the pipe write
    /// ends, so EOF never arrived inside [`COMPLETED_DRAIN_GRACE`]. What the
    /// child wrote before the grace expired is present; anything it wrote after
    /// is absent and will never arrive.
    DrainGraceExpired,
    /// The child wrote more than [`MAX_CAPTURED_BYTES`] on one stream. The
    /// first `MAX_CAPTURED_BYTES` are present; the rest was read and discarded.
    ByteCap,
    /// No reader THREAD could be created for one of the streams, so it was
    /// never read at all. The captured bytes for it are empty — which is a
    /// statement about US, not about what the child printed.
    ReaderUnavailable,
}

/// [`run_with_timeout`] plus the one fact [`TimedOutput`] cannot carry.
#[derive(Debug)]
pub struct TimedRun {
    /// What happened to the child.
    pub outcome: TimedOutput,
    /// `Some` iff the captured stdout/stderr is missing bytes the child (or a
    /// descendant holding its pipes) produced. Always `None` on the
    /// [`TimedOutput::TimedOut`] path, where the output is discarded anyway.
    pub truncation: Option<Truncation>,
}

// ── Children the caller has declared may OUTLIVE the call ───────────────────
//
// The capture door above is the WRONG one for a command whose purpose is to
// leave something running (`fleet`'s `start_command`: `npm run dev`). Two
// independent reasons, both measured, and neither fixable by softening the kill:
//
//  1. **Our own pipes kill the server.** The reader threads OWN the pipe read
//     ends, and the property that makes them safe — they exit when the CALL
//     returns, not when the pipe closes — is exactly what does it: returning
//     drops the read end, and the server holding the inherited write end takes
//     `EPIPE`/`SIGPIPE` on its next log line. `std::process::Command` resets
//     SIGPIPE to `SIG_DFL` in the child, so the default action is *terminate*.
//     This bites hardest on the SUCCESS path, which is the common shape:
//     `sh -c 'npm run dev &'` exits 0 at once, the call returns ~2s later, and
//     the server dies on its next write — after auto-fresh reported success.
//  2. **`/bin/sh` is bash on macOS and RHEL-family, and bash EXECs a simple
//     command.** `bash -c "npm run dev"` has no children at all; the "direct
//     child" is the server itself. "Kill only the child" is not a narrower
//     blast radius there — it is the entire blast radius.
//
// [`start_detached`] answers both by NOT BUILDING the machinery: stdout/stderr
// are `/dev/null`, so there is no pipe, no reader thread, nothing to abandon
// and nothing to SIGPIPE; and the budget bounds the CALLING THREAD only — the
// child is never signalled, exec'd-into or not.
//
// Nothing leaks. Zero reader threads, zero buffered bytes, and the caller's
// thread returns at the deadline. The `Child` handle is parked in
// [`DETACHED_CHILDREN`] and reaped opportunistically, so the eventual exit does
// not become a permanent zombie. Explicitly NOT `mem::forget` on a pipe:
// auto-fresh runs every 300s, which is thousands of leaked fds a week.

/// Children that were left running past their budget, kept ONLY so their exit
/// can be reaped. Never signalled, never read from.
static DETACHED_CHILDREN: std::sync::Mutex<Vec<std::process::Child>> =
    std::sync::Mutex::new(Vec::new());

/// Reap every detached child that has since exited; returns how many are still
/// running. Cheap and non-blocking — `try_wait` per entry, and the list is
/// empty on every device that has never run a `start_command`.
///
/// Opportunistic rather than a background thread ON PURPOSE: a thread that
/// blocks in `waitpid` for a server's whole life is the per-call thread leak
/// this module exists to close, merely relocated. Every bounded run and every
/// [`start_detached`] sweeps first, and the runner issues those constantly, so
/// an exited detached child is reaped within one subprocess call of its exit.
/// The worst case is one zombie pid slot held until the next call — bounded by
/// the number of `start_command`s, not by time.
pub fn reap_detached_children() -> usize {
    let mut g = DETACHED_CHILDREN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // `Err` from `try_wait` means the child is unwaitable (already reaped
    // elsewhere, or a bad handle) — dropping it is the only thing left to do.
    g.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
    g.len()
}

/// How many children [`start_detached`] left running that have not been reaped
/// yet. Exposed so a test can assert on the ledger rather than on timing.
pub fn pending_detached_children() -> usize {
    DETACHED_CHILDREN
        .lock()
        .map(|g| g.len())
        .unwrap_or_else(|poisoned| poisoned.into_inner().len())
}

/// What happened to a child started with [`start_detached`].
///
/// There is no `TimedOut` here on purpose: for a command declared as "may
/// outlive this call", still running at the deadline is the EXPECTED, SUCCESSFUL
/// outcome, not a failure. Reporting it as an `io::Error` is what made a
/// foreground `start_command` fail every auto-fresh cycle it actually worked.
#[derive(Debug)]
pub enum DetachedStart {
    /// The child exited on its own inside the budget — the `sh -c 'npm run dev &'`
    /// shape. Anything it backgrounded keeps running (it is not our child, so
    /// `init` reaps it).
    ///
    /// **Carries no output.** stdout/stderr went to `/dev/null`; see
    /// [`start_detached`] for why that is not negotiable here.
    Exited(std::process::ExitStatus),
    /// The child was still running when the budget expired — the foreground
    /// `npm run dev` shape. It was LEFT ALIVE, untouched, and its exit will be
    /// reaped by [`reap_detached_children`].
    StillRunning {
        /// The still-running child's OS pid, for the caller's log line.
        pid: u32,
    },
}

/// Start a child the caller has declared MAY OUTLIVE this call, and wait up to
/// `budget` to see whether it exits on its own.
///
/// This is the second of the module's two doors — see the block comment above
/// it for why a `start_command` must not go through the capture door.
///
/// What it guarantees:
///
/// - **It returns**, in at most `budget`. That is the only thing the budget
///   bounds; it is a bound on THIS THREAD, never on the child.
/// - **The child is never signalled.** Not at the deadline, not on drop, not by
///   a tree reaper — none is ever armed. A `/bin/sh` that `exec`ed into the
///   server (bash, ksh, zsh) is therefore just as safe as one that forked
///   (dash), which is the difference `TimeoutKill::ChildOnly` could not make.
/// - **stdout and stderr are `/dev/null`**, and stdin is too. No pipe exists,
///   so no reader thread exists, so nothing can be abandoned and nothing can
///   SIGPIPE the child. This is the fix, not a side effect: any capture at all
///   re-creates a read end whose closure kills a long-lived child.
/// - **No zombie.** A child still running at the deadline is parked in
///   [`DETACHED_CHILDREN`] and reaped by the next sweep after it exits.
///
/// The price is that the caller gets NO OUTPUT — not truncated output, none.
/// A caller that needs the server's log must arrange for the command itself to
/// write one (`npm run dev > dev.log 2>&1 &`); it cannot be handed back through
/// a pipe we are not allowed to hold open.
pub fn start_detached(
    mut cmd: std::process::Command,
    budget: std::time::Duration,
    label: &str,
) -> std::io::Result<DetachedStart> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    reap_detached_children();

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd.spawn()?;
    let pid = child.id();

    let deadline = Instant::now() + budget;
    let mut poll = Duration::from_millis(2);
    let max_poll = Duration::from_millis(50);
    loop {
        match child.try_wait()? {
            Some(status) => {
                tracing::debug!(
                    child_pid = pid,
                    ?status,
                    "{label} exited inside its budget (output not captured)"
                );
                return Ok(DetachedStart::Exited(status));
            }
            None => {
                let now = Instant::now();
                if now >= deadline {
                    // INFO, not WARN: this is the success shape for a
                    // foreground server, and it happens on every cycle.
                    tracing::info!(
                        child_pid = pid,
                        budget_secs = budget.as_secs(),
                        "{label} is still running past its budget and was LEFT ALIVE — \
                         this call site declared the child may outlive it"
                    );
                    DETACHED_CHILDREN
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(child);
                    return Ok(DetachedStart::StillRunning { pid });
                }
                std::thread::sleep(poll.min(deadline - now));
                poll = (poll * 2).min(max_poll);
            }
        }
    }
}

/// Run `cmd` to completion, but never for longer than `timeout`, killing the
/// whole process TREE on expiry.
///
/// The right default for a probe. Use [`run_with_timeout_detailed`] to see
/// whether the captured output is complete, and [`start_detached`] — never this
/// — where the command may deliberately leave a long-lived process behind.
///
/// Returns `Err` only when the child could not be spawned or `try_wait` failed.
pub fn run_with_timeout(
    cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<TimedOutput> {
    run_with_timeout_detailed(cmd, timeout).map(|r| r.outcome)
}

/// Run `cmd` to completion, but never for longer than `timeout`.
///
/// Semantics — what this function actually guarantees:
///
/// - **It returns.** Every wait in here is on a hard clock: the child wait is
///   bounded by `timeout`, and BOTH pipe drains are bounded by
///   [`COMPLETED_DRAIN_GRACE`] / [`KILLED_DRAIN_GRACE`]. There is no `join()`
///   on any code path. Worst case is `timeout + COMPLETED_DRAIN_GRACE`.
/// - stdin is `/dev/null` so a child can never block waiting for input (this is
///   also what stops a credential prompt from hanging forever).
/// - stdout/stderr are piped and drained by two detached reader threads from
///   the moment the child exists, so a chatty child cannot deadlock on a full
///   pipe buffer.
/// - **On expiry the whole process TREE is killed and reaped.** A shim
///   (`cmd /c …`, a rustup/volta proxy, a git credential helper) runs the real
///   tool as a grandchild holding both pipe write ends, so killing the child
///   alone leaves it running AND holding our pipes. [`ChildTreeGuard`] is armed
///   before the spawn and fired here.
///
///   **This door is therefore only for a child whose life the call owns.** A
///   command meant to leave a server running goes through [`start_detached`],
///   which pipes nothing and kills nothing; softening the kill here would not
///   help, because our own abandoned readers SIGPIPE the survivor anyway.
/// - **On the success path the tree guard is DISARMED, not fired.** A command
///   that exited 0 may still have left something running, and killing it would
///   be a silent regression.
/// - Reader threads are never joined on ANY path — joining is what
///   re-introduces the hang we are escaping.
///
/// ## What this call can still be holding when it returns
///
/// The bound that matters is that NOTHING here is a function of how long a
/// surviving descendant lives. Concretely, per call:
///
/// | Resource | Worst case |
/// |---|---|
/// | The calling (blocking-pool) thread | `timeout + COMPLETED_DRAIN_GRACE` |
/// | Reader OS threads | 2, for at most one [`READER_ABANDON_POLL`] past the return |
/// | Buffered bytes | `2 * `[`MAX_CAPTURED_BYTES`] (8 MiB), freed at the return |
///
/// The reader threads are the subtle one. Dropping this function's
/// [`PipeDrain`] handles marks them abandoned, and each reader wakes from its
/// bounded wait at least every `READER_ABANDON_POLL` and exits — whether or not
/// EOF ever arrives. That is what lets the success path leave an accidental
/// descendant alive without also leaking two threads and an ever-growing `Vec`
/// per call: before, any child that exited 0 while a grandchild kept the write
/// ends open leaked exactly that, for the grandchild's lifetime, in a process
/// that runs for weeks.
///
/// **What it does NOT make safe is pointing this door at a child that is MEANT
/// to survive.** Abandonment drops the read end, and the survivor holding the
/// write end is then killed by SIGPIPE on its next write. That is
/// [`start_detached`]'s job, not a mode of this one.
///
/// The price of not waiting for EOF is that the captured output can be
/// INCOMPLETE. That is reported in [`TimedRun::truncation`] rather than
/// silently folded into a successful `Output` — see [`Truncation`].
pub fn run_with_timeout_detailed(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<TimedRun> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    // Free any pid slot a previously detached child left behind — this is the
    // sweep that keeps [`start_detached`] thread-free.
    reap_detached_children();

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Pre-spawn half of the tree reaper (Unix process group; no-op on Windows).
    ChildTreeGuard::arm(&mut cmd);

    let mut child = cmd.spawn()?;
    let pid = child.id();
    let tree = ChildTreeGuard::attach_armed(&child);

    let stdout_reader = PipeDrain::spawn(child.stdout.take());
    let stderr_reader = PipeDrain::spawn(child.stderr.take());

    let deadline = Instant::now() + timeout;
    // Back off from a tight poll to a coarse one: a fast `git rev-parse` still
    // returns in ~2ms, while a long-running child costs at most 20 wakeups/s.
    let mut poll = Duration::from_millis(2);
    let max_poll = Duration::from_millis(50);

    loop {
        match child.try_wait()? {
            Some(status) => {
                // Bounded drain, never a join — see the doc comment. If the
                // grace expires, something still holds the write ends; we take
                // the partial read and go, rather than parking this (usually
                // blocking-pool) thread indefinitely.
                let drained = wait_for_drain(&stdout_reader, &stderr_reader, COMPLETED_DRAIN_GRACE);
                let stdout = stdout_reader.snapshot();
                let stderr = stderr_reader.snapshot();
                // Ordered strongest claim first, because more than one can
                // be true at once and the caller only gets to hear one:
                // "nobody read this stream at all" beats "we read it and threw
                // bytes away", which beats "we stopped waiting for the rest".
                let truncation =
                    if stdout_reader.reader_unavailable() || stderr_reader.reader_unavailable() {
                        // The strongest claim of all: a stream nobody read.
                        Some(Truncation::ReaderUnavailable)
                    } else if stdout_reader.was_truncated() || stderr_reader.was_truncated() {
                        Some(Truncation::ByteCap)
                    } else if drained {
                        None
                    } else {
                        Some(Truncation::DrainGraceExpired)
                    };
                if let Some(reason) = truncation {
                    // WARN, not debug: this is the difference between "here is
                    // the output" and "here is SOME of the output", and a
                    // caller that mistakes one for the other produces a
                    // confident wrong answer (see `Truncation`).
                    tracing::warn!(
                        child_pid = pid,
                        program = %program_label(&cmd),
                        ?reason,
                        stdout_bytes = stdout.len(),
                        stderr_bytes = stderr.len(),
                        "child exited but its output could not be read in full; \
                         the captured stdout/stderr is INCOMPLETE"
                    );
                }
                // Release the tree WITHOUT killing it: this command succeeded
                // on its own terms and may have left something running.
                tree.disarm();
                return Ok(TimedRun {
                    outcome: TimedOutput::Completed(std::process::Output {
                        status,
                        stdout,
                        stderr,
                    }),
                    truncation,
                });
            }
            None => {
                let now = Instant::now();
                if now >= deadline {
                    let _ = child.kill();
                    let reaped = child.wait().is_ok();
                    // Fire the reaper, killing any grandchild the child left
                    // behind — including one holding our pipe write ends.
                    drop(tree);
                    // Bounded, and not a join: the readers noticing the EOF we
                    // just caused. If something we could not reach still holds
                    // a write end, abandonment (`PipeDrain::drop`) releases
                    // them instead.
                    let _ = wait_for_drain(&stdout_reader, &stderr_reader, KILLED_DRAIN_GRACE);
                    return Ok(TimedRun {
                        outcome: TimedOutput::TimedOut { pid, reaped },
                        truncation: None,
                    });
                }
                std::thread::sleep(poll.min(deadline - now));
                poll = (poll * 2).min(max_poll);
            }
        }
    }
}

/// The ONLY thing this module will ever put in a log line or an error string
/// to identify a command: its program name. **Never the arguments.**
///
/// Argv is where credentials live. `setup_wizard::github_clone_repo` runs
/// `git clone https://x-access-token:<TOKEN>@github.com/o/r.git`, and
/// `new_project` runs `git remote set-url origin <same>`; both call sites go to
/// explicit lengths to redact the token out of *their* strings, and both were
/// defeated by a helper that formatted `{:?}` of the whole `Command` into a
/// WARN and into `io::Error` before the caller ever saw it — landing the token
/// in `qontinui-runner.log` (retained 14 days) and in the wizard's UI error.
///
/// Reducing the label to the program name makes that leak impossible **by
/// construction** rather than by convention: the argv is not merely redacted
/// here, it is never read at all, so no future caller can leak one by
/// forgetting to scrub. A caller that wants more context passes its OWN label
/// to [`output_with_timeout_labeled`] or [`run_probe`] — text it authored,
/// which therefore cannot contain a secret it did not choose to put there.
fn program_label(cmd: &std::process::Command) -> String {
    let program = cmd.get_program();
    std::path::Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy()
        .into_owned()
}

/// Drop-in bounded replacement for `Command::output()`.
///
/// Returns exactly what [`std::process::Command::output`] returns, so a call
/// site converts by wrapping the built command and leaving every downstream
/// arm (`.ok()?`, `.and_then(|o| …)`, `match { Ok(o) if o.status.success() … }`)
/// untouched — EXCEPT that a child which overruns `timeout` is killed (with its
/// whole tree), reaped, and surfaced as `Err(ErrorKind::TimedOut)` instead of
/// parking the calling thread forever.
///
/// The timeout WARN and the returned error name the PROGRAM only, never the
/// argv — see [`program_label`]. Use [`output_with_timeout_labeled`] when the
/// program name alone is too vague to diagnose from.
///
/// Use this where the caller already has bespoke handling for `Output` and a
/// timeout is honestly just one more way to fail; use [`run_probe`] where the
/// caller only wants stdout-or-degrade. Use [`start_detached`] where the
/// command may deliberately leave a process running past its budget — it is a
/// different door, not a flag on this one.
pub fn output_with_timeout(
    cmd: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    let label = program_label(&cmd);
    output_with_timeout_labeled(cmd, timeout, &label)
}

/// [`output_with_timeout`] with a caller-authored label instead of the bare
/// program name.
///
/// `label` is used verbatim in the WARN and in the error message, so it must be
/// text the caller wrote (e.g. `"wrappers: node --manifest-only"`) — never a
/// formatted command line, and never anything derived from a token, URL or
/// user-supplied argument.
/// Truncation note: a child whose output could not be read in full still comes
/// back as `Ok(Output)` here, with a WARN naming the reason — this shape's
/// callers use `Output` for an exit status and a message, and turning a
/// truncated build log into an `Err` would report a successful build as failed.
/// A caller that derives a VERDICT from the bytes must use [`run_probe`] (which
/// degrades on truncation) or [`run_with_timeout_detailed`] (which reports it).
pub fn output_with_timeout_labeled(
    cmd: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> std::io::Result<std::process::Output> {
    match run_with_timeout_detailed(cmd, timeout).map(|r| r.outcome) {
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
    /// The child exited 0 inside the budget, but its stdout could NOT be read
    /// in full — see [`Truncation`].
    ///
    /// A degrade rather than a `Captured` with fewer bytes, because every
    /// caller of this shape derives a VERDICT from the bytes
    /// (`agent_worktree::dirty` reads `git status --porcelain` and calls an
    /// empty result *clean*, which then permits removal). A partial read that
    /// happens to contain no dirty lines is not evidence of a clean tree; it is
    /// evidence of nothing, which is exactly what `Degraded` means.
    Truncated(Truncation),
}

/// Outcome of one bounded external probe — see [`run_probe`].
#[derive(Debug)]
pub enum ProbeOutcome {
    /// The child exited 0 inside the budget; carries its raw stdout, IN FULL.
    ///
    /// "In full" is load-bearing: a stdout that was cut short by the drain
    /// grace or the byte cap is [`DegradeReason::Truncated`], never this.
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
/// module and the operation (e.g. `"process_tree: PowerShell snapshot"`), and
/// nothing derived from a token or a URL.
///
/// Use [`run_probe_quiet`] where a non-zero exit is an EXPECTED, routine answer
/// (`lsof` on an unowned port, `taskkill` on an already-dead pid) — WARNing on
/// those buries the timeout WARN that actually matters.
pub fn run_probe(
    cmd: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> ProbeOutcome {
    run_probe_inner(cmd, timeout, label, true)
}

/// [`run_probe`], but a non-zero exit or a spawn failure is logged at DEBUG
/// instead of WARN.
///
/// For probes whose negative answer is ordinary and frequent: `lsof -t -i:<port>`
/// exits 1 whenever nothing owns the port, `ss -ltnp` is simply absent on macOS
/// and on minimal images, `netstat | findstr` exits 1 when not listening, and
/// `taskkill /F /T /PID` exits 128 for a pid that already went away. All four
/// leave the caller's verdict unchanged, and all four fire on a per-process
/// timer — so WARNing on them is pure log volume that hides the one line this
/// PR exists to add.
///
/// A **timeout is still WARNed**, at full volume: that one is never expected,
/// and the killed pid plus the budget are what make the next incident
/// diagnosable from the log alone. So is a TRUNCATED read, for the same reason:
/// it means this probe's answer was withheld, which is exactly the fact a
/// silent degrade would hide.
pub fn run_probe_quiet(
    cmd: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
) -> ProbeOutcome {
    run_probe_inner(cmd, timeout, label, false)
}

fn run_probe_inner(
    cmd: std::process::Command,
    timeout: std::time::Duration,
    label: &str,
    warn_on_expected_failure: bool,
) -> ProbeOutcome {
    // Probes never intentionally start anything, so a surviving descendant on
    // this path is always an orphan holding our pipes — which is exactly what
    // this door's unconditional tree-kill is for.
    let run = run_with_timeout_detailed(cmd, timeout);
    match run {
        // Truncation is checked BEFORE success, on purpose: a zero exit status
        // says the child finished, not that we read what it wrote.
        Ok(TimedRun {
            outcome: TimedOutput::Completed(o),
            truncation: Some(reason),
        }) if o.status.success() => {
            tracing::warn!(
                ?reason,
                stdout_bytes = o.stdout.len(),
                "{label} exited 0 but its stdout is INCOMPLETE — degrading this pass \
                 rather than answering from a partial read"
            );
            ProbeOutcome::Degraded(DegradeReason::Truncated(reason))
        }
        Ok(TimedRun {
            outcome: TimedOutput::Completed(o),
            ..
        }) if o.status.success() => ProbeOutcome::Captured(o.stdout),
        Ok(TimedRun {
            outcome: TimedOutput::Completed(o),
            ..
        }) => {
            if warn_on_expected_failure {
                tracing::warn!(
                    "{label} failed (status={:?}, stderr={})",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                );
            } else {
                tracing::debug!(
                    "{label} returned a negative answer (status={:?}, stderr={})",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            ProbeOutcome::Degraded(DegradeReason::Status)
        }
        Ok(TimedRun {
            outcome: TimedOutput::TimedOut { pid, reaped },
            ..
        }) => {
            // WARN, not debug: a silent timeout just relocates the mystery.
            // The killed pid + the budget are what make the next incident
            // diagnosable from the log alone. Loud even in the quiet variant.
            tracing::warn!(
                child_pid = pid,
                reaped,
                timeout_secs = timeout.as_secs(),
                "{label} timed out and was killed — degrading this pass"
            );
            ProbeOutcome::Degraded(DegradeReason::TimedOut { pid, reaped })
        }
        Err(e) => {
            if warn_on_expected_failure {
                tracing::warn!("{label} spawn error: {e}");
            } else {
                tracing::debug!("{label} is not available here: {e}");
            }
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
            elapsed < budget * 8 + KILLED_DRAIN_GRACE,
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
            elapsed < budget * 8 + KILLED_DRAIN_GRACE,
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

    /// The quiet variant must degrade IDENTICALLY — only the log level differs.
    #[test]
    fn run_probe_quiet_degrades_the_same_way_on_a_negative_answer() {
        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = no_window("cmd.exe");
            c.args(["/C", "exit 1"]);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = no_window("sh");
            c.args(["-c", "exit 1"]);
            c
        };
        assert!(matches!(
            run_probe_quiet(cmd, Duration::from_secs(20), "test: quiet failing probe"),
            ProbeOutcome::Degraded(DegradeReason::Status)
        ));

        // A binary that does not exist is a SpawnError, still a degrade.
        let missing = no_window("qontinui-no-such-binary-9f2a1c");
        assert!(matches!(
            run_probe_quiet(
                missing,
                Duration::from_secs(5),
                "test: quiet missing binary"
            ),
            ProbeOutcome::Degraded(DegradeReason::SpawnError)
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
        assert!(started.elapsed() < budget * 8 + KILLED_DRAIN_GRACE);
    }

    // ── Credential-leak regression (2026-08-30 review, CRITICAL 4) ───────────

    /// A pretend GitHub installation token, in exactly the shape
    /// `setup_wizard::github_clone_repo` puts on git's command line.
    const FAKE_TOKEN: &str = "ghs_TOTALLYNOTAREALTOKEN0123456789";

    /// The same command shape as a tokenised `git clone`: it hangs, and the
    /// secret is in its argv.
    fn secret_bearing_sleeper() -> std::process::Command {
        #[cfg(target_os = "windows")]
        {
            let mut c = no_window("cmd.exe");
            // `rem` never runs (ping blocks for ~59s) but the token IS in argv.
            c.args([
                "/C",
                &format!("ping -n 60 127.0.0.1 & rem x-access-token:{FAKE_TOKEN}"),
            ]);
            c
        }
        #[cfg(not(target_os = "windows"))]
        {
            let mut c = no_window("sh");
            // Extra arg lands in `$0`; the shell still just sleeps.
            c.args([
                "-c",
                "sleep 60",
                &format!("https://x-access-token:{FAKE_TOKEN}@github.com/o/r.git"),
            ]);
            c
        }
    }

    /// **The credential leak, in miniature.** Before the fix,
    /// `output_with_timeout` built its label as `format!("{:?}", cmd)` — the
    /// full argv — and put it in BOTH a `tracing::warn!` and the returned
    /// `io::Error`, so a `git clone` killed at its 900s budget wrote the
    /// installation token into `qontinui-runner.log` and into the wizard's
    /// user-facing error string.
    #[test]
    fn output_with_timeout_never_leaks_argv_into_its_error() {
        let cmd = secret_bearing_sleeper();

        // (a) The label is what goes into the WARN *and* the error, so
        //     asserting on it covers the log line too — there is exactly one
        //     string, and it is built from the program name alone.
        let label = program_label(&cmd);
        assert!(
            !label.contains(FAKE_TOKEN) && !label.contains("x-access-token"),
            "the log/error label must not carry argv; got {label:?}"
        );
        assert!(
            label.starts_with("sh") || label.starts_with("cmd"),
            "the label must still identify the program; got {label:?}"
        );

        // (b) End to end: force the timeout and read the error the caller sees.
        let err = output_with_timeout(cmd, Duration::from_millis(300))
            .expect_err("a hung child must be Err");
        let msg = err.to_string();
        assert!(
            !msg.contains(FAKE_TOKEN),
            "the token leaked into the error string: {msg:?}"
        );
        assert!(
            !msg.contains("x-access-token"),
            "the authenticated URL leaked into the error string: {msg:?}"
        );
        assert!(
            msg.contains("exceeded its"),
            "the error must still say what happened; got {msg:?}"
        );
    }

    /// A caller-authored label is passed through verbatim — that is the only
    /// way extra context can reach the log, and it can only contain what the
    /// caller chose to write.
    #[test]
    fn a_caller_supplied_label_is_used_verbatim() {
        let err = output_with_timeout_labeled(
            secret_bearing_sleeper(),
            Duration::from_millis(300),
            "wrappers: node --manifest-only",
        )
        .expect_err("a hung child must be Err");
        let msg = err.to_string();
        assert!(
            msg.contains("wrappers: node --manifest-only"),
            "got {msg:?}"
        );
        assert!(!msg.contains(FAKE_TOKEN), "got {msg:?}");
    }

    // ── Surviving-grandchild regression (2026-08-30 review, CRITICAL 3) ──────

    /// Serialises the two reader-thread-gauge tests.
    ///
    /// [`live_pipe_readers`] is process-global and `cargo test` runs these in
    /// parallel threads. The completed-path test DELIBERATELY leaves two
    /// readers alive for its grandchild's lifetime (that is the documented
    /// trade-off), which would otherwise read as a leak in the timeout-path
    /// test. Every other test in this module holds readers for well under a
    /// second, so the lock only has to keep these two apart.
    static GAUGE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Poll — bounded — for the module's live reader-thread gauge to fall back
    /// to `target`. Returns the final reading either way, so the assertion can
    /// report the real number rather than just "timed out".
    fn wait_for_readers(target: usize, budget: Duration) -> usize {
        let deadline = Instant::now() + budget;
        loop {
            let n = live_pipe_readers();
            if n <= target || Instant::now() >= deadline {
                return n;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A child that leaves a GRANDCHILD holding the inherited pipes, and then
    /// exits 0 itself.
    ///
    /// `sh -c 'sleep 8 & exit 0'`: the backgrounded `sleep` inherits stdout
    /// and stderr, so `read_to_end` on those pipes NEVER sees EOF even though
    /// the direct child is already gone. This is the `cmd /c` shim shape, and
    /// the shape a `git` that spawns a credential/askpass helper takes. Eight
    /// seconds is comfortably longer than `COMPLETED_DRAIN_GRACE`, which is all
    /// the assertion needs.
    #[cfg(unix)]
    fn completed_with_surviving_grandchild() -> std::process::Command {
        let mut c = no_window("sh");
        c.args(["-c", "sleep 8 & exit 0"]);
        c
    }

    /// A child that hangs AND leaves a grandchild holding the pipes.
    ///
    /// `sh -c 'sleep 60 & exec sleep 60'` — `exec` makes the outer shell
    /// *become* the second sleep, so killing "the child" is one `SIGKILL` on a
    /// process whose sibling still owns both write ends.
    #[cfg(unix)]
    fn hung_with_surviving_grandchild() -> std::process::Command {
        let mut c = no_window("sh");
        c.args(["-c", "sleep 60 & exec sleep 60"]);
        c
    }

    /// **CRITICAL 3, completed path.** Against the pre-fix code this test does
    /// not merely fail — it HANGS for the grandchild's full 30s, because
    /// `stdout_reader.join()` is unbounded and the pipes never EOF. That hang
    /// happened inside a `spawn_blocking_tracked` body in production, i.e. on
    /// the path that reports SUCCESS.
    ///
    /// Evidence asserted, not timing alone:
    /// - the call returns inside the documented `COMPLETED_DRAIN_GRACE` bound;
    /// - the exit status is the real one (we degrade the *output*, not the
    ///   verdict);
    /// - the two reader threads are accounted for via the module's own live
    ///   gauge rather than a sleep.
    #[cfg(unix)]
    #[test]
    fn a_completed_child_with_a_surviving_grandchild_does_not_block_forever() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let before = live_pipe_readers();
        let started = Instant::now();
        let out = run_with_timeout(
            completed_with_surviving_grandchild(),
            Duration::from_secs(20),
        )
        .expect("spawn");
        let elapsed = started.elapsed();

        match out {
            TimedOutput::Completed(o) => {
                assert!(
                    o.status.success(),
                    "the direct child exited 0; the grandchild must not change that"
                );
            }
            TimedOutput::TimedOut { .. } => {
                panic!("the direct child exits immediately — this must not be a timeout")
            }
        }

        // The whole point: bounded by the drain grace, NOT by the grandchild's
        // 30s lifetime and NOT forever.
        assert!(
            elapsed < COMPLETED_DRAIN_GRACE + Duration::from_secs(3),
            "the completed path blocked for {elapsed:?} waiting on a grandchild-held pipe"
        );

        // The success path deliberately does NOT kill the surviving
        // descendant (see `run_with_timeout`'s doc), so its readers stay up
        // until it exits — but they must be exactly the two this call made,
        // and they must go away on their own. 20s covers the 8s sleep. Held
        // under `GAUGE_TESTS` so the timeout-path test does not see them.
        let after = wait_for_readers(before, Duration::from_secs(20));
        assert!(
            after <= before,
            "reader threads leaked: {before} before, {after} after"
        );
    }

    /// **CRITICAL 3, timeout path.** Before the fix, `child.kill()` killed one
    /// process while the sibling `sleep` kept both pipe write ends open, so the
    /// two detached reader threads never saw EOF and were leaked PERMANENTLY —
    /// two OS threads and one orphaned process per timeout, on the path that is
    /// supposed to be the recovery.
    ///
    /// Evidence asserted: the reader-thread gauge returns to its pre-call
    /// value, which can only happen if the whole tree was reaped.
    #[cfg(unix)]
    #[test]
    fn a_timed_out_child_reaps_its_whole_tree_and_leaks_no_reader_threads() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let before = live_pipe_readers();
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let out = run_with_timeout(hung_with_surviving_grandchild(), budget).expect("spawn");
        let elapsed = started.elapsed();

        match out {
            TimedOutput::TimedOut { pid, reaped } => {
                assert!(pid > 0);
                assert!(reaped, "the timed-out child must be reaped");
            }
            TimedOutput::Completed(o) => panic!("expected a timeout, got {:?}", o.status),
        }
        assert!(
            elapsed < budget * 8 + KILLED_DRAIN_GRACE,
            "the timeout path took {elapsed:?} against a {budget:?} budget"
        );

        // The load-bearing assertion. The backgrounded `sleep 60` is the only
        // thing that could still hold the pipes; if the tree reaper did not
        // fire, these two readers stay blocked in `read` for a full minute and
        // this gauge never comes back down inside the window below.
        let after = wait_for_readers(before, Duration::from_secs(10));
        assert!(
            after <= before,
            "the surviving grandchild kept {} reader thread(s) alive after the timeout \
             ({before} before, {after} after) — the child tree was not reaped",
            after.saturating_sub(before)
        );
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

    // ── Drain-leak regression (2026-08-30 re-review, CRITICAL 1) ────────────

    /// Is `pid` still a live process? `kill(pid, 0)` performs the permission
    /// and existence checks and delivers nothing.
    #[cfg(unix)]
    fn pid_alive(pid: i32) -> bool {
        // SAFETY: signal 0 is the documented "existence check" no-op.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// **CRITICAL 1, thread half.** The success path used to leak TWO OS
    /// threads per call whose child left a descendant on the pipes: the readers
    /// looped until EOF, and EOF cannot arrive while a descendant holds the
    /// write ends. A `git` that spawned a credential/askpass helper, or any
    /// `cmd /c` shim whose grandchild outlives it, is exactly that shape — so
    /// the leak was two threads per such call, in a process that runs for weeks.
    ///
    /// (The auto-fresh `start_command` that first exposed this no longer comes
    /// through here at all: it goes through [`start_detached`], because
    /// abandoning a reader is *fatal* to a child that is meant to survive. This
    /// test covers the ACCIDENTAL survivor, which nothing can declare away.)
    ///
    /// Evidence, not timing: the module's own live-reader gauge must come back
    /// to its pre-call value on a schedule set by the CALL
    /// (`COMPLETED_DRAIN_GRACE + READER_ABANDON_POLL`), not by the descendant's
    /// 20s lifetime. Against the pre-fix code the gauge stays up for the full
    /// 20s and the window below expires with the readers still counted.
    #[cfg(unix)]
    #[test]
    fn a_completed_child_abandons_its_readers_instead_of_outliving_the_call() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let before = live_pipe_readers();

        let mut c = no_window("sh");
        // The backgrounded sleep inherits both pipe write ends and holds them
        // for 20s — far beyond any bound this call is allowed to have.
        c.args(["-c", "sleep 20 & exit 0"]);
        let out = run_with_timeout(c, Duration::from_secs(20)).expect("spawn");
        assert!(
            matches!(out, TimedOutput::Completed(ref o) if o.status.success()),
            "the direct child exits 0 immediately; got {out:?}"
        );

        // The load-bearing bound: one drain grace plus one abandonment poll,
        // plus slack for a loaded CI box — and nothing that scales with the
        // descendant.
        let window = COMPLETED_DRAIN_GRACE + READER_ABANDON_POLL + Duration::from_secs(3);
        assert!(
            window < Duration::from_secs(20),
            "this assertion is only meaningful while the window is well under \
             the descendant's lifetime"
        );
        let after = wait_for_readers(before, window);
        assert!(
            after <= before,
            "reader threads outlived their call: {before} before, {after} after {window:?}. \
             They are waiting for an EOF a surviving descendant will never send."
        );
    }

    /// **CRITICAL 1, memory half.** `g.extend_from_slice(&chunk[..n])` had no
    /// cap, so the same surviving descendant also grew an unbounded `Vec` that
    /// nobody would ever read.
    ///
    /// Asserts the bound directly (buffer size), and that the shortfall is
    /// REPORTED rather than passed off as the child's complete output.
    #[cfg(unix)]
    #[test]
    fn a_flood_of_stdout_is_capped_and_reported_as_truncated() {
        let mut c = no_window("sh");
        // 5 MiB — comfortably over the 4 MiB cap, and instant.
        c.args(["-c", "dd if=/dev/zero bs=65536 count=80 2>/dev/null"]);
        let run = run_with_timeout_detailed(c, Duration::from_secs(60)).expect("spawn");

        match run.outcome {
            TimedOutput::Completed(ref o) => {
                assert!(o.status.success(), "dd must succeed");
                assert_eq!(
                    o.stdout.len(),
                    MAX_CAPTURED_BYTES,
                    "the drain buffer must stop at the cap, not grow with the child"
                );
            }
            ref other => panic!("dd must not time out; got {other:?}"),
        }
        assert_eq!(
            run.truncation,
            Some(Truncation::ByteCap),
            "a capped read must SAY it is incomplete"
        );

        // The child must still have been allowed to finish: a reader that
        // stopped draining at the cap would leave it blocked on a full pipe,
        // turning "produced a lot of output" into "timed out and was killed".
        let mut c2 = no_window("sh");
        c2.args(["-c", "dd if=/dev/zero bs=65536 count=80 2>/dev/null"]);
        assert!(
            matches!(
                run_probe(c2, Duration::from_secs(60), "test: flood"),
                ProbeOutcome::Degraded(DegradeReason::Truncated(Truncation::ByteCap))
            ),
            "run_probe must degrade on a capped read rather than hand back a partial stdout"
        );
    }

    /// **HIGH 3.** A truncated-but-successful read used to be indistinguishable
    /// from a complete one: `drained == false` was logged at DEBUG, discarded,
    /// and the caller got `Completed` with `status.success() == true` and a
    /// PARTIAL stdout. `run_probe` mapped that to `Captured`, and
    /// `agent_worktree::dirty` maps a `Captured` payload with no dirty lines to
    /// `DirtyVerdict::Clean` — a definite verdict derived from bytes never read.
    #[cfg(unix)]
    #[test]
    fn a_partial_read_degrades_the_probe_instead_of_answering_from_it() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut c = no_window("sh");
        // Writes something, exits 0, and leaves a descendant on the pipes so
        // EOF never arrives inside COMPLETED_DRAIN_GRACE.
        c.args(["-c", "echo hello; sleep 20 & exit 0"]);

        match run_probe(c, Duration::from_secs(30), "test: partial read") {
            ProbeOutcome::Degraded(DegradeReason::Truncated(Truncation::DrainGraceExpired)) => {}
            ProbeOutcome::Captured(bytes) => panic!(
                "a partial read was presented as the child's complete stdout: {:?}",
                String::from_utf8_lossy(&bytes)
            ),
            other => panic!("expected a Truncated degrade, got {other:?}"),
        }
    }

    // ── The detached door (2026-08-30 round-3 review, CRITICAL) ─────────────

    /// A "server" that actually WRITES to stdout, forever, and leaves a
    /// witness of every write in `$TICKFILE`.
    ///
    /// Writing to stdout is the property the round-2 stand-in (`sleep 30`) did
    /// not have, and the reason the round-2 test could not see the defect:
    /// SIGPIPE is only delivered on a WRITE, so a server that never writes
    /// survives having its pipe read end closed underneath it. Every real
    /// `start_command` — `npm run dev`, `uvicorn`, `cargo run` — logs on a loop.
    ///
    /// The `$TICKFILE` witness is what makes the ASSERTION honest. `kill(pid, 0)`
    /// succeeds for a ZOMBIE too, and a detached child that we killed is exactly
    /// that until the next reap sweep — so pid existence alone cannot tell
    /// "still serving" from "killed and not yet reaped". A tick count that keeps
    /// RISING can only come from a process that is still running and still able
    /// to write. stdout is written FIRST, so a SIGPIPE lands before the witness.
    ///
    /// Passed by environment rather than interpolated, so it survives the nested
    /// quoting of the `exec` case below.
    #[cfg(unix)]
    const TICKER: &str = r#"while :; do echo tick; echo tick >> "$TICKFILE"; sleep 0.2; done"#;

    /// Long enough for the ticker to have written many lines after the call
    /// returned — i.e. long enough for a SIGPIPE to have landed.
    #[cfg(unix)]
    const SIGPIPE_WINDOW: Duration = Duration::from_secs(3);

    /// How many ticks the witness file has recorded so far. Absent file == 0:
    /// the server may not have reached its first write yet.
    #[cfg(unix)]
    fn tick_count(tickfile: &std::path::Path) -> usize {
        std::fs::read_to_string(tickfile)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// The ticks a live server writes in [`SIGPIPE_WINDOW`] is ~15 (one per
    /// 200ms). Requiring a good fraction of that keeps the assertion about
    /// "still serving" rather than about one straggling buffered line.
    #[cfg(unix)]
    const MIN_TICKS_IN_WINDOW: usize = 5;

    #[cfg(unix)]
    fn read_pid(pidfile: &std::path::Path) -> i32 {
        std::fs::read_to_string(pidfile)
            .expect("the start command must have recorded its server pid")
            .trim()
            .parse()
            .expect("pidfile must hold a pid")
    }

    /// A command whose whole purpose is to leave a server running, in the shape
    /// a FOREGROUND `start_command` takes: it records the server's pid and then
    /// never exits itself, so it always reaches the timeout path.
    #[cfg(unix)]
    fn foreground_start_command(pidfile: &std::path::Path) -> std::process::Command {
        let mut c = no_window("sh");
        c.args([
            "-c",
            &format!(
                "sleep 30 & echo $! > '{}'; exec sleep 30",
                pidfile.display()
            ),
        ]);
        c
    }

    /// **CRITICAL, success path — the one that bites in production.**
    ///
    /// `sh -c 'npm run dev &'` exits 0 immediately, so this never reaches any
    /// timeout logic at all: the call burns `COMPLETED_DRAIN_GRACE`, returns,
    /// and the pipe readers are abandoned. Abandonment DROPS the read ends, and
    /// the backgrounded server holding the inherited write end then dies of
    /// SIGPIPE on its next log line — `std::process::Command` resets SIGPIPE to
    /// `SIG_DFL` in the child, so the default action is terminate. Measured at
    /// ~2.25s after auto-fresh had already reported "started successfully".
    ///
    /// `start_detached` closes it by never creating the pipe. Evidence asserted:
    /// the server's own pid is still live a full `SIGPIPE_WINDOW` after the call
    /// returned, during which it wrote ~15 lines; and the module's live-reader
    /// gauge shows no reader thread was ever created for it.
    #[cfg(unix)]
    #[test]
    fn a_backgrounding_start_command_leaves_its_ticking_server_alive() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let before_readers = live_pipe_readers();
        let dir = tempfile::tempdir().expect("tempdir");
        let pidfile = dir.path().join("server.pid");

        let tickfile = dir.path().join("ticks");
        let mut c = no_window("sh");
        c.env("TICKFILE", &tickfile);
        c.env("PIDFILE", &pidfile);
        c.args(["-c", &format!(r#"{TICKER} & echo $! > "$PIDFILE""#)]);

        match start_detached(c, Duration::from_secs(20), "test: backgrounding start")
            .expect("spawn")
        {
            DetachedStart::Exited(status) => assert!(
                status.success(),
                "`<server> &` exits 0 at once; got {status:?}"
            ),
            other => panic!("the backgrounding shape must exit, not run on: {other:?}"),
        }

        // The mechanism, asserted directly: no pipe ⇒ no reader thread ⇒
        // nothing that could be abandoned ⇒ nothing that could SIGPIPE.
        let after_readers = wait_for_readers(before_readers, Duration::from_secs(5));
        assert!(
            after_readers <= before_readers,
            "start_detached created reader thread(s) ({before_readers} -> {after_readers}); \
             their read ends are what SIGPIPE the server"
        );

        let server = read_pid(&pidfile);
        let ticks_at_return = tick_count(&tickfile);
        std::thread::sleep(SIGPIPE_WINDOW);
        let alive = pid_alive(server);
        let ticks_later = tick_count(&tickfile);
        // SAFETY: our own descendant. Killed before the assertions so a failure
        // does not leave a ticker behind.
        unsafe { libc::kill(server, libc::SIGKILL) };
        assert!(
            alive,
            "the server this command exists to start (pid {server}) died within \
             {SIGPIPE_WINDOW:?} of the call returning — our abandoned pipe readers \
             closed the read end and SIGPIPE killed it"
        );
        assert!(
            ticks_later >= ticks_at_return + MIN_TICKS_IN_WINDOW,
            "the server (pid {server}) stopped writing after the call returned \
             ({ticks_at_return} -> {ticks_later} ticks in {SIGPIPE_WINDOW:?}): its stdout \
             is a pipe whose read end we closed, so the next `echo` took SIGPIPE"
        );
    }

    /// **CRITICAL, timeout path — and the `/bin/sh`-is-bash exec case.**
    ///
    /// `bash -c "npm run dev"` EXECs: the shell becomes the server, so there is
    /// no separate "just the shell" left to kill at the deadline. `sh` is bash
    /// on macOS and the RHEL family, and dash (which forks) on Debian/Ubuntu —
    /// so the previous test's multi-command script could not represent the case
    /// at all. Spelling `exec` makes this test the exec case on EVERY shell:
    /// the pid `start_detached` reports IS the ticker.
    ///
    /// Evidence: that pid is still live a `SIGPIPE_WINDOW` after the call
    /// returned, which requires both halves of the fix — nothing killed it, and
    /// nothing SIGPIPEd it.
    #[cfg(unix)]
    #[test]
    fn a_foreground_start_command_that_exec_ed_into_the_server_is_left_alive() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let before_pending = pending_detached_children();

        let dir = tempfile::tempdir().expect("tempdir");
        let tickfile = dir.path().join("ticks");
        let mut c = no_window("sh");
        c.env("TICKFILE", &tickfile);
        // `exec` makes the shell BECOME the ticker; the env survives the exec.
        c.args(["-c", &format!("exec sh -c '{TICKER}'")]);

        let pid = match start_detached(c, Duration::from_millis(600), "test: foreground start")
            .expect("spawn")
        {
            DetachedStart::StillRunning { pid } => pid,
            other => panic!("a foreground server must still be running: {other:?}"),
        };
        let pid = i32::try_from(pid).expect("pid fits an i32");

        // The child is parked for reaping rather than forgotten — that is what
        // keeps "never kill it" from meaning "leak a zombie pid slot".
        assert!(
            pending_detached_children() > before_pending,
            "a still-running detached child must be recorded for reaping"
        );

        let ticks_at_return = tick_count(&tickfile);
        std::thread::sleep(SIGPIPE_WINDOW);
        let alive = pid_alive(pid);
        let ticks_later = tick_count(&tickfile);
        // SAFETY: our own child, killed so the test leaves nothing behind.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        assert!(
            alive,
            "the exec'd server (pid {pid}) did not survive its own start_command: it was \
             either killed at the deadline or SIGPIPEd by our abandoned pipe readers"
        );
        // The load-bearing half. A child we killed is a ZOMBIE until the next
        // reap sweep, and `kill(pid, 0)` cannot tell that from a live server —
        // only the witness file can.
        assert!(
            ticks_later >= ticks_at_return + MIN_TICKS_IN_WINDOW,
            "the exec'd server (pid {pid}) stopped serving after the call returned \
             ({ticks_at_return} -> {ticks_later} ticks in {SIGPIPE_WINDOW:?}) — it was \
             signalled at the deadline, or SIGPIPEd by our own pipe readers"
        );

        // And the ledger drains once it is gone — no permanent zombie.
        let deadline = Instant::now() + Duration::from_secs(5);
        while pending_detached_children() > before_pending && Instant::now() < deadline {
            reap_detached_children();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            pending_detached_children() <= before_pending,
            "an exited detached child was never reaped: {} still pending",
            pending_detached_children()
        );
    }

    /// The other half: the CAPTURE door must STILL reap everything. Without
    /// this, "stop killing the server" could quietly become "never tree-kill",
    /// which reinstates the orphan-plus-held-pipes defect the tree guard exists
    /// for.
    #[cfg(unix)]
    #[test]
    fn a_tree_timeout_still_reaps_the_whole_group() {
        let _serial = GAUGE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let pidfile = dir.path().join("server.pid");

        let run = run_with_timeout_detailed(
            foreground_start_command(&pidfile),
            Duration::from_millis(500),
        )
        .expect("spawn");
        assert!(matches!(run.outcome, TimedOutput::TimedOut { .. }));

        let server = read_pid(&pidfile);
        let deadline = Instant::now() + Duration::from_secs(5);
        while pid_alive(server) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !pid_alive(server),
            "the process group was not reaped: pid {server} survived a bounded run"
        );
    }

    // ── EINTR regression (2026-08-30 round-3 review, HIGH) ──────────────────

    /// Does nothing. Installed only so a signal is DELIVERED rather than
    /// terminating the test process.
    #[cfg(unix)]
    extern "C" fn noop_signal_handler(_sig: libc::c_int) {}

    /// **HIGH.** `poll(2)` is explicitly NOT restarted by `SA_RESTART`
    /// (signal(7)), and this crate installs exactly such a handler for SIGCHLD
    /// the moment any `tokio::process::Child` is spawned. `wait_readable_raw`
    /// mapped every `rc != 0` — EINTR included — to `Ready`, which sends the
    /// reader straight into the UNINTERRUPTIBLE `read` the mechanism exists to
    /// avoid. The reader's lifetime then reverts to the PIPE's rather than the
    /// CALL's: precisely the leak this module was written to close.
    ///
    /// Evidence, not timing alone: the wait returns `TimedOut` (so the reader
    /// re-checks its abandoned flag and loops) AND it returns early, which
    /// proves the interruption actually happened rather than the 5s budget
    /// simply expiring.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_wait_reports_timed_out_instead_of_ready() {
        use std::sync::mpsc;

        // An SA_RESTART handler, installed the way signal_hook_registry/tokio
        // install the SIGCHLD one. SIGUSR1 is used so nothing else in the test
        // binary is disturbed.
        // SAFETY: a zeroed sigaction filled in per sigaction(2); the handler is
        // an `extern "C"` fn that touches nothing.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = noop_signal_handler as *const () as libc::sighandler_t;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(std::ptr::addr_of_mut!(sa.sa_mask));
            assert_eq!(
                libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()),
                0,
                "could not install the test signal handler"
            );
        }

        // An EMPTY pipe: without a signal, `poll` blocks for the whole budget.
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: a two-element array, exactly what pipe(2) writes into.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2) failed");
        let (rd, wr) = (fds[0], fds[1]);

        let (tid_tx, tid_rx) = mpsc::channel::<usize>();
        let (res_tx, res_rx) = mpsc::channel::<(bool, Duration)>();
        let waiter = std::thread::spawn(move || {
            // SAFETY: reads this thread's own pthread id.
            let _ = tid_tx.send(unsafe { libc::pthread_self() } as usize);
            let started = Instant::now();
            let ready = matches!(
                wait_readable_raw(rd, Duration::from_secs(5)),
                PipeReady::Ready
            );
            let _ = res_tx.send((ready, started.elapsed()));
        });
        let tid = tid_rx.recv().expect("waiter must report its thread id") as libc::pthread_t;

        // Signal repeatedly until the waiter answers, so the test does not
        // depend on winning a race with the thread entering `poll`.
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut answer = None;
        while Instant::now() < deadline {
            // SAFETY: a live thread of this process, and a handled signal.
            unsafe { libc::pthread_kill(tid, libc::SIGUSR1) };
            if let Ok(v) = res_rx.recv_timeout(Duration::from_millis(50)) {
                answer = Some(v);
                break;
            }
        }
        let (reported_ready, elapsed) = answer.expect("the interrupted wait never returned");
        waiter.join().expect("waiter thread must not panic");
        // SAFETY: our own fds, and the handler is restored to the default.
        unsafe {
            libc::close(rd);
            libc::close(wr);
            libc::signal(libc::SIGUSR1, libc::SIG_DFL);
        }

        assert!(
            elapsed < Duration::from_secs(4),
            "the wait ran its full budget ({elapsed:?}) — it was never interrupted, so this \
             test proves nothing"
        );
        assert!(
            !reported_ready,
            "an EINTR was reported as READY on a pipe with no data: the reader would now \
             block in an uninterruptible `read` for the pipe's lifetime, not the call's"
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

    /// A chatty child must still have ALL of its output captured — the shared
    /// buffer replaced a `read_to_end` that returned the whole thing, so this
    /// guards against the bounded drain silently truncating normal output.
    #[test]
    fn a_chatty_child_has_all_of_its_output_captured() {
        #[cfg(target_os = "windows")]
        let cmd = {
            let mut c = no_window("cmd.exe");
            c.args(["/C", "for /L %i in (1,1,2000) do @echo line-%i"]);
            c
        };
        #[cfg(not(target_os = "windows"))]
        let cmd = {
            let mut c = no_window("sh");
            c.args([
                "-c",
                "i=1; while [ $i -le 2000 ]; do echo line-$i; i=$((i+1)); done",
            ]);
            c
        };

        match run_with_timeout(cmd, Duration::from_secs(60)).expect("spawn") {
            TimedOutput::Completed(o) => {
                assert!(o.status.success());
                let s = String::from_utf8_lossy(&o.stdout);
                assert_eq!(
                    s.lines().filter(|l| l.trim().starts_with("line-")).count(),
                    2000,
                    "the bounded drain must not truncate a normal child's output"
                );
            }
            TimedOutput::TimedOut { .. } => panic!("a bounded loop must not time out"),
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
