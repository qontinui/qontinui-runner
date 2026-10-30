//! A SIGTERM or SIGHUP is a shutdown request: mark the runner as shutting down
//! inside the signal handler, then run the SAME graceful quit the X button and
//! `POST /ui-bridge/control/page/force-close` run
//! (`mcp::ai_session::emergency_quit::request_force_close`), so a service stop
//! (`systemctl --user stop`, which sends SIGTERM) ends with a clean shutdown
//! marker and restorable session records instead of reading as a crash.
//!
//! **A raw signal handler, never a runtime listener.** A `tokio::signal`
//! listener replaces the default action with "notify the runtime", so a wedged
//! or starved runtime would never act on SIGTERM. Nothing here depends on the
//! tokio runtime. The handler performs only async-signal-safe operations: an
//! atomic load/store/swap, `write(2)`, `alarm(2)`, `sigaction(2)` and
//! `raise(3)`. On the first delivery it marks quitting, writes the signal
//! number as one byte to a self-pipe, arms the kernel backstop and RETURNS. A
//! plain OS thread (`signal-quit-watch`), blocked in `read(2)` on the other end
//! of that pipe, wakes and does the work a handler must not do: it stamps the
//! clean-shutdown marker FIRST (a requested stop is a planned stop even if
//! something cuts it short — the supervisor's 1.5 s SIGKILL grace, a host's
//! shorter `TimeoutStopSec`), then calls `request_force_close`.
//!
//! **The kernel keeps the termination guarantee.** Three properties replace the
//! old "re-raise at once":
//!
//! 1. **Backstop.** The handler calls `alarm(BACKSTOP_SECS)`. `SIGALRM`'s
//!    default action ends the process even if the watcher never runs.
//!    `BACKSTOP_SECS` is `shutdown_budget::FORCE_EXIT_BUDGET` + 2 s, computed
//!    from the budget rather than restated, and it stays under the primary
//!    unit's `TimeoutStopSec=30`. A death by SIGALRM is an honest record: the
//!    stop did not finish its graceful path inside the budget.
//! 2. **A second signal is immediate.** A one-shot latch (`DELIVERED`) sends
//!    every delivery after the first — of either signal, by any route —
//!    straight to `SIG_DFL` + `raise`. It does not rely on `SA_RESETHAND`
//!    alone: that resets only the disposition of the signal that was delivered,
//!    so a SIGHUP after a SIGTERM, or a chained invocation (another component
//!    registered over this handler and calls it without `SA_RESETHAND`), would
//!    otherwise re-enter the armed path and call `alarm` again, pushing the
//!    backstop out on every repeat.
//! 3. **Unarmed means the old behaviour.** If the pipe could not be created or
//!    the watcher thread could not be spawned (or the watcher's `read` failed
//!    and it exited), the write end is `-1` and the handler re-raises at once,
//!    exactly as before. A disarmed install never leaves a process that ignores
//!    SIGTERM.
//!
//! Installed only over a `SIG_DFL` disposition: a signal the process inherited
//! as ignored (`nohup`, `cmd &` without job control) stays ignored, and one
//! some other component already handles is left to it. SIGINT is not touched:
//! the `restate` feature's HTTP endpoint listens for Ctrl-C through tokio, and
//! taking it over would change what Ctrl-C does there.

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use tracing::{info, warn};

    /// The signals this module handles: a shutdown request in every sense.
    pub const SIGNALS: [libc::c_int; 2] = [libc::SIGTERM, libc::SIGHUP];

    /// The kernel backstop armed on the first delivery. Computed from the
    /// force-close budget so widening the budget widens it.
    pub const BACKSTOP_SECS: libc::c_uint =
        (crate::shutdown_budget::FORCE_EXIT_BUDGET.as_secs() + 2) as libc::c_uint;
    const _: () = assert!(
        BACKSTOP_SECS as u64 > crate::shutdown_budget::FORCE_EXIT_BUDGET.as_secs(),
        "the backstop must outlast the graceful quit's own hard-exit budget"
    );

    /// The self-pipe's write end, or `-1` when the graceful path is unarmed.
    static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
    /// Set on the first delivery of any shutdown signal (D4 property 2).
    static DELIVERED: AtomicBool = AtomicBool::new(false);

    /// PURE: whether to install over a signal whose current disposition is
    /// `current`. Only the default: never over an inherited `SIG_IGN`, and
    /// never over a handler some other component installed.
    pub fn should_install(current: libc::sighandler_t) -> bool {
        current == libc::SIG_DFL
    }

    /// The handler's effects, injected so its decision logic is testable
    /// without the production constant or a real watcher. Every effect must be
    /// async-signal-safe.
    pub struct Effects {
        pub mark_quitting: fn(),
        pub reraise: fn(libc::c_int),
        /// Write the wake byte for `sig` to `fd`; `true` when exactly one byte
        /// was written.
        pub wake: fn(fd: libc::c_int, sig: libc::c_int) -> bool,
        pub arm_backstop: fn(libc::c_uint),
        pub backstop_secs: libc::c_uint,
    }

    /// The handler's whole work. Marks quitting FIRST (an atomic store), then:
    /// a repeat delivery, or an unarmed handler, re-raises for the default
    /// action; the armed first delivery writes the wake byte and arms the
    /// backstop, then returns. A failed write re-raises too — nothing would
    /// wake the watcher, so dying now is the only honest outcome.
    pub fn handle(
        sig: libc::c_int,
        delivered: &AtomicBool,
        wake_fd: &AtomicI32,
        fx: &Effects,
    ) {
        (fx.mark_quitting)();
        let repeat = delivered.swap(true, Ordering::SeqCst);
        let fd = wake_fd.load(Ordering::SeqCst);
        if repeat || fd < 0 {
            (fx.reraise)(sig);
            return;
        }
        if !(fx.wake)(fd, sig) {
            (fx.reraise)(sig);
            return;
        }
        (fx.arm_backstop)(fx.backstop_secs);
    }

    /// Set the default disposition explicitly, then raise the same signal.
    ///
    /// SAFETY of the body: `sigaction` and `raise` are async-signal-safe. The
    /// default disposition is set EXPLICITLY, not left to `SA_RESETHAND`: a
    /// component that chains to this handler would otherwise re-enter it with
    /// the disposition still pointing at the chain (review round 12 N2). Also
    /// called from the watcher thread, where the same two calls end the
    /// process by the signal.
    pub fn reraise_default(sig: libc::c_int) {
        unsafe {
            let mut dfl: libc::sigaction = std::mem::zeroed();
            dfl.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut dfl.sa_mask);
            libc::sigaction(sig, &dfl, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    /// Write `sig` as one byte to `fd`. Async-signal-safe.
    pub fn write_wake(fd: libc::c_int, sig: libc::c_int) -> bool {
        let byte = sig as u8;
        // SAFETY: a one-byte write from a live stack slot.
        unsafe { libc::write(fd, &byte as *const u8 as *const libc::c_void, 1) == 1 }
    }

    fn arm_alarm(secs: libc::c_uint) {
        // SAFETY: `alarm` is async-signal-safe and takes no pointers.
        unsafe {
            libc::alarm(secs);
        }
    }

    static PRODUCTION: Effects = Effects {
        mark_quitting: crate::commands::terminal_windows::mark_app_quitting,
        reraise: reraise_default,
        wake: write_wake,
        arm_backstop: arm_alarm,
        backstop_secs: BACKSTOP_SECS,
    };

    pub(super) extern "C" fn on_signal(sig: libc::c_int) {
        handle(sig, &DELIVERED, &WAKE_FD, &PRODUCTION);
    }

    /// The signal's name, for logs and the force-close reason.
    pub fn name(sig: libc::c_int) -> String {
        match sig {
            libc::SIGTERM => "SIGTERM".to_string(),
            libc::SIGHUP => "SIGHUP".to_string(),
            other => format!("signal {other}"),
        }
    }

    /// The watcher's work after one wake byte, with its effects injected so the
    /// ORDER is testable: stamp the clean marker FIRST, then resolve the app;
    /// no app (an early-boot signal) means the old behaviour from thread
    /// context; a refused force-close means another door owns the exit (the
    /// backstop still bounds it).
    pub fn on_wake<A>(
        sig: libc::c_int,
        stamp_clean: impl FnOnce(),
        resolve: impl FnOnce() -> Option<A>,
        request_quit: impl FnOnce(A, String) -> bool,
        reraise: impl FnOnce(libc::c_int),
    ) {
        stamp_clean();
        match resolve() {
            None => reraise(sig),
            Some(app) => {
                if !request_quit(app, format!("signal {}", name(sig))) {
                    info!(
                        "{}: a teardown is already in progress — the other door owns the exit",
                        name(sig)
                    );
                }
            }
        }
    }

    /// Block on `read_fd` until one byte arrives, then hand it to `on_byte`.
    /// EOF or a hard read error disarms (`wake_fd` → `-1`, so the handler
    /// re-raises at once) and returns: a dead watcher must never be waited on.
    pub fn watch(read_fd: libc::c_int, wake_fd: &AtomicI32, on_byte: impl FnOnce(libc::c_int)) {
        loop {
            let mut byte = 0u8;
            // SAFETY: a one-byte read into a live stack slot.
            let n = unsafe { libc::read(read_fd, &mut byte as *mut u8 as *mut libc::c_void, 1) };
            if n == 1 {
                on_byte(byte as libc::c_int);
                return;
            }
            if n < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            let why = if n == 0 {
                "EOF".to_string()
            } else {
                std::io::Error::last_os_error().to_string()
            };
            wake_fd.store(-1, Ordering::SeqCst);
            warn!("signal-quit-watch: read failed ({why}) — graceful signal quit disarmed");
            return;
        }
    }

    fn production_wake(sig: libc::c_int) {
        use tauri::Manager;
        warn!(
            "{} received — stamping a clean shutdown and starting the graceful quit",
            name(sig)
        );
        on_wake(
            sig,
            || {
                crate::session::shutdown_marker::mark_clean_shutdown(
                    &crate::session::shutdown_marker::marker_path(),
                )
            },
            || {
                let app = crate::tauri_app_handle::current()?;
                let state = app
                    .try_state::<std::sync::Arc<crate::commands::AppState>>()?
                    .inner()
                    .clone();
                Some((app, state))
            },
            |(app, state), reason| {
                crate::mcp::ai_session::emergency_quit::request_force_close(app, state, reason)
            },
            reraise_default,
        );
    }

    /// Create the self-pipe and spawn the watcher. On any failure the handler
    /// stays unarmed (re-raise at once) and one `warn!` says why.
    fn arm_watcher() {
        let mut fds: [libc::c_int; 2] = [-1; 2];
        // SAFETY: `pipe` fills two descriptors; `fcntl` only edits their flags.
        let ok = unsafe {
            libc::pipe(fds.as_mut_ptr()) == 0
                && libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && {
                    let fl = libc::fcntl(fds[1], libc::F_GETFL);
                    fl >= 0 && libc::fcntl(fds[1], libc::F_SETFL, fl | libc::O_NONBLOCK) == 0
                }
        };
        if !ok {
            let err = std::io::Error::last_os_error();
            close_all(&fds);
            warn!("shutdown signals: self-pipe setup failed ({err}) — a SIGTERM ends the process at once, without the graceful quit");
            return;
        }
        let read_fd = fds[0];
        let spawned = std::thread::Builder::new()
            .name("signal-quit-watch".to_string())
            .spawn(move || watch(read_fd, &WAKE_FD, production_wake));
        if let Err(err) = spawned {
            close_all(&fds);
            warn!("shutdown signals: watcher thread spawn failed ({err}) — a SIGTERM ends the process at once, without the graceful quit");
            return;
        }
        WAKE_FD.store(fds[1], Ordering::SeqCst);
    }

    fn close_all(fds: &[libc::c_int; 2]) {
        for &fd in fds {
            if fd >= 0 {
                // SAFETY: closing a descriptor this function opened.
                unsafe {
                    libc::close(fd);
                }
            }
        }
    }

    /// The current disposition of `sig`, or `None` when it cannot be read.
    fn current(sig: libc::c_int) -> Option<libc::sighandler_t> {
        // SAFETY: reading the disposition only (null new action).
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            (libc::sigaction(sig, std::ptr::null(), &mut old) == 0).then_some(old.sa_sigaction)
        }
    }

    /// Install `on_signal` for every signal in [`SIGNALS`] whose disposition is
    /// the default. Arms nothing: without [`arm_watcher`] the handler re-raises.
    pub fn install_handlers() {
        for sig in SIGNALS {
            if !current(sig).is_some_and(should_install) {
                continue;
            }
            // SAFETY: a zeroed sigaction filled in per sigaction(2); the
            // handler only performs async-signal-safe operations.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                sa.sa_flags = libc::SA_RESETHAND;
                libc::sigemptyset(&mut sa.sa_mask);
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
        }
    }

    pub fn install() {
        // Arm first so the handler never sees a half-armed state; an unarmed
        // handler re-raises, so either order is safe.
        arm_watcher();
        install_handlers();
    }
}

/// Install the SIGTERM/SIGHUP graceful-quit handler (unix; no-op elsewhere).
pub fn install() {
    #[cfg(unix)]
    imp::install();
}

/// Test-only: the handler's address, for a child that chains to it.
#[cfg(all(test, unix))]
fn handler_for_test() -> libc::sighandler_t {
    imp::on_signal as *const () as libc::sighandler_t
}

/// Test-only: install the handlers into a forked child, UNARMED. Never
/// `imp::install`: that spawns a thread, which must not happen after `fork`
/// in a multi-threaded test binary.
#[cfg(all(test, unix))]
fn install_for_test() {
    imp::install_handlers();
}

#[cfg(all(test, unix))]
mod tests {
    use super::imp::*;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    /// Review M2: never over an inherited `SIG_IGN` (a `nohup`'d runner must
    /// keep ignoring SIGHUP), never over another component's handler; only the
    /// default disposition is taken over.
    #[test]
    fn the_marker_installs_only_over_the_default_disposition() {
        assert!(should_install(libc::SIG_DFL));
        assert!(!should_install(libc::SIG_IGN));
        assert!(!should_install(0x1234 as libc::sighandler_t));
        assert!(!SIGNALS.contains(&libc::SIGINT), "Ctrl-C is restate's");
    }

    fn noop() {}
    fn no_wake(_fd: libc::c_int, _sig: libc::c_int) -> bool {
        false
    }
    fn no_alarm(_secs: libc::c_uint) {}

    /// The unarmed handler path marks quitting, and does so BEFORE re-raising
    /// (the re-raise ends the process, so a mark after it would never happen).
    #[test]
    fn the_handler_path_sets_the_flag_before_reraising() {
        static MARKED: AtomicBool = AtomicBool::new(false);
        static RAISED: AtomicI32 = AtomicI32::new(0);
        static MARKED_AT_RAISE: AtomicBool = AtomicBool::new(false);
        let fx = Effects {
            mark_quitting: || MARKED.store(true, Ordering::SeqCst),
            reraise: |sig| {
                MARKED_AT_RAISE.store(MARKED.load(Ordering::SeqCst), Ordering::SeqCst);
                RAISED.store(sig, Ordering::SeqCst);
            },
            wake: no_wake,
            arm_backstop: no_alarm,
            backstop_secs: 0,
        };
        let delivered = AtomicBool::new(false);
        let unarmed = AtomicI32::new(-1);
        handle(libc::SIGTERM, &delivered, &unarmed, &fx);
        assert!(MARKED.load(Ordering::SeqCst));
        assert_eq!(RAISED.load(Ordering::SeqCst), libc::SIGTERM);
        assert!(
            MARKED_AT_RAISE.load(Ordering::SeqCst),
            "mark precedes the re-raise"
        );
    }

    /// The armed first delivery writes the byte, arms the backstop and does
    /// NOT re-raise; a write that fails re-raises instead of arming.
    #[test]
    fn the_armed_handler_wakes_then_arms_and_a_failed_write_reraises() {
        static RAISES: AtomicI32 = AtomicI32::new(0);
        static ALARMED: AtomicI32 = AtomicI32::new(0);
        let ok = Effects {
            mark_quitting: noop,
            reraise: |_| {
                RAISES.fetch_add(1, Ordering::SeqCst);
            },
            wake: |_, _| true,
            arm_backstop: |s| ALARMED.store(s as i32, Ordering::SeqCst),
            backstop_secs: 27,
        };
        handle(libc::SIGTERM, &AtomicBool::new(false), &AtomicI32::new(9), &ok);
        assert_eq!(RAISES.load(Ordering::SeqCst), 0, "armed first delivery returns");
        assert_eq!(ALARMED.load(Ordering::SeqCst), 27, "backstop armed");

        let failing = Effects { wake: no_wake, ..ok };
        ALARMED.store(0, Ordering::SeqCst);
        handle(libc::SIGTERM, &AtomicBool::new(false), &AtomicI32::new(9), &failing);
        assert_eq!(RAISES.load(Ordering::SeqCst), 1, "a failed wake re-raises");
        assert_eq!(ALARMED.load(Ordering::SeqCst), 0, "and never arms the backstop");
    }

    /// The watcher stamps the clean marker BEFORE it reaches the app lookup,
    /// and with no app it re-raises the same signal.
    #[test]
    fn the_watcher_stamps_before_the_app_lookup_and_reraises_without_an_app() {
        let order = std::cell::RefCell::new(Vec::new());
        on_wake::<()>(
            libc::SIGHUP,
            || order.borrow_mut().push("stamp"),
            || {
                order.borrow_mut().push("resolve");
                None
            },
            |_, _| {
                order.borrow_mut().push("quit");
                true
            },
            |sig| order.borrow_mut().push(if sig == libc::SIGHUP { "reraise-hup" } else { "reraise-other" }),
        );
        assert_eq!(*order.borrow(), vec!["stamp", "resolve", "reraise-hup"]);
    }

    /// With an app the watcher calls the force-close with a `signal <NAME>`
    /// reason; a refused force-close (latch held) does not re-raise.
    #[test]
    fn the_watcher_hands_off_to_the_force_close_and_respects_a_held_latch() {
        let reason = std::cell::RefCell::new(String::new());
        let raised = std::cell::Cell::new(false);
        on_wake(
            libc::SIGTERM,
            || {},
            || Some(7u8),
            |app, r| {
                assert_eq!(app, 7);
                *reason.borrow_mut() = r;
                false
            },
            |_| raised.set(true),
        );
        assert_eq!(*reason.borrow(), "signal SIGTERM");
        assert!(!raised.get(), "a held latch means another door owns the exit");
    }

    /// The watcher hands one byte to its callback; EOF disarms the handler.
    #[test]
    fn the_watcher_reads_one_byte_and_eof_disarms() {
        let mut fds: [libc::c_int; 2] = [-1; 2];
        // SAFETY: a pipe for this test only.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert!(write_wake(fds[1], libc::SIGTERM));
        let wake = AtomicI32::new(fds[1]);
        let got = std::cell::Cell::new(0);
        watch(fds[0], &wake, |b| got.set(b));
        assert_eq!(got.get(), libc::SIGTERM);
        assert_eq!(wake.load(Ordering::SeqCst), fds[1], "a byte does not disarm");

        // SAFETY: closing this test's own write end.
        unsafe { libc::close(fds[1]) };
        let called = std::cell::Cell::new(false);
        watch(fds[0], &wake, |_| called.set(true));
        assert!(!called.get());
        assert_eq!(wake.load(Ordering::SeqCst), -1, "EOF disarms");
        // SAFETY: closing this test's own read end.
        unsafe { libc::close(fds[0]) };
    }

    /// Run `child` in a forked process and return its wait status. The child
    /// only makes async-signal-safe calls and always ends with `_exit`, so it
    /// never returns into the test harness.
    fn in_child(child: fn()) -> libc::c_int {
        // SAFETY: fork in a test; the child runs only `child` (sigaction,
        // sigprocmask, pipe, read, write, kill, an atomic store) and `_exit`s.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                // A child that deadlocks must die signalled rather than hang
                // the parent's waitpid: SIGALRM after 10 s, armed FIRST. A child
                // test whose handler arms its own shorter alarm replaces this.
                libc::alarm(10);
                std::panic::set_hook(Box::new(|_| {}));
                if std::panic::catch_unwind(child).is_err() {
                    libc::_exit(99);
                }
                libc::_exit(7);
            }
            let mut status = 0;
            loop {
                let r = libc::waitpid(pid, &mut status, 0);
                if r == pid {
                    break;
                }
                assert!(
                    r == -1
                        && std::io::Error::last_os_error().kind()
                            == std::io::ErrorKind::Interrupted,
                    "waitpid failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            status
        }
    }

    fn unblock(sig: libc::c_int) {
        // SAFETY: async-signal-safe mask edit in the forked child.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, sig);
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        }
    }

    fn set_disposition_for(sig: libc::c_int, handler: libc::sighandler_t, flags: libc::c_int) {
        // SAFETY: async-signal-safe disposition change in the forked child.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler;
            sa.sa_flags = flags;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }

    fn set_disposition(handler: libc::sighandler_t) {
        set_disposition_for(libc::SIGTERM, handler, 0);
    }

    fn kill_self(sig: libc::c_int) {
        // SAFETY: signalling ourselves.
        unsafe {
            libc::kill(libc::getpid(), sig);
        }
    }

    /// Review round 12 N4: a real SIGTERM still kills the process BY that
    /// signal once the UNARMED handler is installed, and an inherited `SIG_IGN`
    /// is still honoured.
    #[test]
    fn a_real_sigterm_still_ends_the_process_by_that_signal() {
        let status = in_child(|| {
            unblock(libc::SIGTERM);
            set_disposition(libc::SIG_DFL);
            super::install_for_test();
            kill_self(libc::SIGTERM);
        });
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGTERM);

        let status = in_child(|| {
            unblock(libc::SIGTERM);
            set_disposition(libc::SIG_IGN);
            super::install_for_test();
            kill_self(libc::SIGTERM);
        });
        assert!(
            libc::WIFEXITED(status),
            "ignored SIGTERM: status {status:#x}"
        );
        assert_eq!(libc::WEXITSTATUS(status), 7);
    }

    /// Review round 12 N2: an UNARMED handler invoked WITHOUT `SA_RESETHAND`
    /// — as when another component chains to it — still ends the process by
    /// the signal, not re-raising into itself forever.
    #[test]
    fn a_chained_invocation_still_ends_by_the_signal() {
        let status = in_child(|| {
            unblock(libc::SIGTERM);
            set_disposition(super::handler_for_test());
            // SAFETY: a bounded alarm.
            unsafe {
                libc::alarm(2);
            }
            kill_self(libc::SIGTERM);
        });
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(
            libc::WTERMSIG(status),
            libc::SIGTERM,
            "the handler looped until the alarm"
        );
    }

    // ── Armed forked-child tests ────────────────────────────────────────────
    //
    // Each armed test installs its OWN handler: the production `handle` with a
    // test-only 1 s backstop injected through `Effects`, never the production
    // constant. Statics are per test because each child writes its own copy
    // after `fork`; the parent never touches them except where noted.

    const TEST_BACKSTOP: libc::c_uint = 1;

    fn test_fx() -> Effects {
        Effects {
            mark_quitting: noop,
            reraise: reraise_default,
            wake: write_wake,
            arm_backstop: |s| unsafe {
                libc::alarm(s);
            },
            backstop_secs: TEST_BACKSTOP,
        }
    }

    /// Make a pipe in the child; store the write end in `wake`; return read end.
    fn child_pipe(wake: &AtomicI32) -> libc::c_int {
        let mut fds: [libc::c_int; 2] = [-1; 2];
        // SAFETY: async-signal-safe pipe + fcntl in the forked child.
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                libc::_exit(40);
            }
            let fl = libc::fcntl(fds[0], libc::F_GETFL);
            libc::fcntl(fds[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        wake.store(fds[1], Ordering::SeqCst);
        fds[0]
    }

    /// Read every byte available on a non-blocking `fd`: `(count, all equal
    /// to want)`. Never exits — the parent calls it too.
    fn drain(fd: libc::c_int, want: libc::c_int) -> (usize, bool) {
        let (mut n, mut all_match) = (0, true);
        loop {
            let mut b = 0u8;
            // SAFETY: a one-byte read into a live stack slot.
            let r = unsafe { libc::read(fd, &mut b as *mut u8 as *mut libc::c_void, 1) };
            if r != 1 {
                return (n, all_match);
            }
            all_match &= b as libc::c_int == want;
            n += 1;
        }
    }

    fn pause_forever() -> ! {
        loop {
            // SAFETY: waits for a signal; the backstop or a kill ends us.
            unsafe {
                libc::pause();
            }
        }
    }

    macro_rules! armed_suite {
        ($modname:ident, $sig:expr) => {
            mod $modname {
                use super::*;

                static D: AtomicBool = AtomicBool::new(false);
                static W: AtomicI32 = AtomicI32::new(-1);
                extern "C" fn h(sig: libc::c_int) {
                    handle(sig, &D, &W, &test_fx());
                }

                /// Armed, no reader on the pipe: the backstop ends the child
                /// by SIGALRM within backstop + 1 s.
                #[test]
                fn armed_with_no_reader_dies_by_the_backstop() {
                    let t0 = Instant::now();
                    let status = in_child(|| {
                        unblock($sig);
                        let _ = child_pipe(&W);
                        set_disposition_for($sig, h as *const () as libc::sighandler_t, libc::SA_RESETHAND);
                        kill_self($sig);
                        pause_forever();
                    });
                    let took = t0.elapsed();
                    assert!(libc::WIFSIGNALED(status), "status {status:#x}");
                    assert_eq!(libc::WTERMSIG(status), libc::SIGALRM);
                    assert!(
                        took < Duration::from_secs(TEST_BACKSTOP as u64 + 1),
                        "died after {took:?}, not by the injected backstop"
                    );
                }

                /// Armed: exactly one byte, equal to the signal number.
                #[test]
                fn armed_writes_exactly_one_byte_equal_to_the_signal() {
                    let status = in_child(|| {
                        unblock($sig);
                        let r = child_pipe(&W);
                        set_disposition_for($sig, h as *const () as libc::sighandler_t, libc::SA_RESETHAND);
                        kill_self($sig);
                        let (n, all_match) = drain(r, $sig);
                        // SAFETY: cancel the injected backstop before exiting.
                        unsafe { libc::alarm(0) };
                        if n != 1 || !all_match {
                            unsafe { libc::_exit(42) };
                        }
                    });
                    assert!(libc::WIFEXITED(status), "status {status:#x}");
                    assert_eq!(libc::WEXITSTATUS(status), 7, "exit 42 = wrong byte count or value");
                }

                /// Armed: a second delivery ends the child by the signal at once.
                #[test]
                fn armed_second_signal_is_immediate() {
                    let t0 = Instant::now();
                    let status = in_child(|| {
                        unblock($sig);
                        let _ = child_pipe(&W);
                        set_disposition_for($sig, h as *const () as libc::sighandler_t, libc::SA_RESETHAND);
                        kill_self($sig);
                        kill_self($sig);
                        pause_forever();
                    });
                    assert!(libc::WIFSIGNALED(status), "status {status:#x}");
                    assert_eq!(libc::WTERMSIG(status), $sig);
                    assert!(t0.elapsed() < Duration::from_secs(1));
                }
            }
        };
    }

    armed_suite!(armed_sigterm, libc::SIGTERM);
    armed_suite!(armed_sighup, libc::SIGHUP);

    /// D4 property 2's cross-signal case: SIGTERM then SIGHUP — two separate
    /// dispositions, so `SA_RESETHAND` alone would let the SIGHUP re-enter the
    /// armed path. The latch makes it die by SIGHUP at once, with one byte.
    #[test]
    fn armed_sigterm_then_sighup_dies_by_sighup_with_one_byte() {
        static D: AtomicBool = AtomicBool::new(false);
        static W: AtomicI32 = AtomicI32::new(-1);
        static R: AtomicI32 = AtomicI32::new(-1);
        extern "C" fn h(sig: libc::c_int) {
            handle(sig, &D, &W, &test_fx());
        }
        // The PARENT makes this pipe so it can read it after the child dies.
        let mut fds: [libc::c_int; 2] = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe {
            let fl = libc::fcntl(fds[0], libc::F_GETFL);
            libc::fcntl(fds[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        R.store(fds[0], Ordering::SeqCst);
        W.store(fds[1], Ordering::SeqCst);
        let status = in_child(|| {
            unblock(libc::SIGTERM);
            unblock(libc::SIGHUP);
            for sig in [libc::SIGTERM, libc::SIGHUP] {
                set_disposition_for(sig, h as *const () as libc::sighandler_t, libc::SA_RESETHAND);
            }
            kill_self(libc::SIGTERM);
            kill_self(libc::SIGHUP);
            pause_forever();
        });
        W.store(-1, Ordering::SeqCst);
        unsafe { libc::close(fds[1]) };
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGHUP);
        assert_eq!(drain(R.load(Ordering::SeqCst), libc::SIGTERM), (1, true), "exactly the first byte");
        unsafe { libc::close(fds[0]) };
    }

    /// A chained invocation of the ARMED handler (no `SA_RESETHAND`): the first
    /// delivery writes one byte and returns; the second ends the child by the
    /// signal, and writes nothing more.
    #[test]
    fn armed_chained_invocation_wakes_once_then_dies_by_the_signal() {
        static D: AtomicBool = AtomicBool::new(false);
        static W: AtomicI32 = AtomicI32::new(-1);
        static R: AtomicI32 = AtomicI32::new(-1);
        extern "C" fn h(sig: libc::c_int) {
            handle(sig, &D, &W, &test_fx());
        }
        let mut fds: [libc::c_int; 2] = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe {
            let fl = libc::fcntl(fds[0], libc::F_GETFL);
            libc::fcntl(fds[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        R.store(fds[0], Ordering::SeqCst);
        W.store(fds[1], Ordering::SeqCst);
        let status = in_child(|| {
            unblock(libc::SIGTERM);
            set_disposition_for(libc::SIGTERM, h as *const () as libc::sighandler_t, 0);
            kill_self(libc::SIGTERM);
            kill_self(libc::SIGTERM);
            pause_forever();
        });
        W.store(-1, Ordering::SeqCst);
        unsafe { libc::close(fds[1]) };
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGTERM);
        assert_eq!(drain(R.load(Ordering::SeqCst), libc::SIGTERM), (1, true));
        unsafe { libc::close(fds[0]) };
    }

    /// The watcher with no app handle, in a real process: it re-raises from
    /// thread context, so the child dies by the signal.
    #[test]
    fn the_watcher_without_an_app_ends_the_process_by_the_signal() {
        let status = in_child(|| {
            unblock(libc::SIGTERM);
            on_wake::<()>(libc::SIGTERM, || {}, || None, |_, _| true, reraise_default);
        });
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGTERM);
    }
}
