//! Mark the runner as shutting down when SIGTERM or SIGHUP arrives, so the
//! panes that die in that shutdown keep their restart-shaped close
//! (`session_lifecycle_store::witnessed_close_reason`) instead of reading as
//! a `claude` that ended under a live runner.
//!
//! **A raw signal handler, never a runtime listener.** A `tokio::signal`
//! listener replaces the default action with "notify the runtime", so a
//! wedged or starved runtime would no longer die on SIGTERM (the supervisor
//! then escalates to SIGKILL, systemd waits out its stop timeout) and Ctrl-C on
//! a wedged runner would do nothing. Here the handler itself only stores an
//! atomic flag — async-signal-safe — and the kernel's default action then ends
//! the process exactly as before: the handler is installed with
//! `SA_RESETHAND` (the disposition resets to `SIG_DFL` on delivery) and
//! re-raises the same signal, which is delivered as soon as it returns.
//!
//! Installed only over a `SIG_DFL` disposition: a signal the process inherited
//! as ignored (`nohup`, `cmd &` without job control) stays ignored, and one
//! some other component already handles is left to it. SIGINT is not touched:
//! the `restate` feature's HTTP endpoint listens for Ctrl-C through tokio, and
//! taking it over would change what Ctrl-C does there.

#[cfg(unix)]
mod imp {
    /// The signals this module marks: a shutdown request in every sense.
    pub const SIGNALS: [libc::c_int; 2] = [libc::SIGTERM, libc::SIGHUP];

    /// PURE: whether to install over a signal whose current disposition is
    /// `current`. Only the default: never over an inherited `SIG_IGN`, and
    /// never over a handler some other component installed.
    pub fn should_install(current: libc::sighandler_t) -> bool {
        current == libc::SIG_DFL
    }

    /// The handler's whole work, with its two effects injected so it is
    /// testable without delivering a signal: mark quitting (an atomic store,
    /// async-signal-safe) FIRST, then re-raise for the default action.
    pub fn handle(sig: libc::c_int, mark_quitting: fn(), reraise: fn(libc::c_int)) {
        mark_quitting();
        reraise(sig);
    }

    fn reraise_default(sig: libc::c_int) {
        // SAFETY: `sigaction` and `raise` are async-signal-safe. The default
        // disposition is set EXPLICITLY before re-raising, not left to
        // `SA_RESETHAND` alone: a component that later chains to this handler
        // (signal-hook registering over it and calling the previous action)
        // would otherwise re-enter it with the disposition still pointing at
        // the chain, and re-raise into itself forever (review round 12 N2).
        // The re-raised signal is blocked while the handler runs and delivered
        // with the default action as it returns — the process ends exactly as
        // it did with no handler.
        unsafe {
            let mut dfl: libc::sigaction = std::mem::zeroed();
            dfl.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut dfl.sa_mask);
            libc::sigaction(sig, &dfl, std::ptr::null_mut());
            libc::raise(sig);
        }
    }

    pub(super) extern "C" fn on_signal(sig: libc::c_int) {
        handle(
            sig,
            crate::commands::terminal_windows::mark_app_quitting,
            reraise_default,
        );
    }

    /// The current disposition of `sig`, or `None` when it cannot be read.
    fn current(sig: libc::c_int) -> Option<libc::sighandler_t> {
        // SAFETY: reading the disposition only (null new action).
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            (libc::sigaction(sig, std::ptr::null(), &mut old) == 0).then_some(old.sa_sigaction)
        }
    }

    pub fn install() {
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
}

/// Install the shutdown marker for SIGTERM and SIGHUP (unix; no-op elsewhere).
pub fn install() {
    #[cfg(unix)]
    imp::install();
}

/// Test-only: the handler's address, for a child that chains to it.
#[cfg(all(test, unix))]
fn handler_for_test() -> libc::sighandler_t {
    imp::on_signal as *const () as libc::sighandler_t
}

/// Test-only: install into a forked child.
#[cfg(all(test, unix))]
fn install_for_test() {
    imp::install();
}

#[cfg(all(test, unix))]
mod tests {
    use super::imp::*;

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

    /// The handler path marks quitting, and does so BEFORE re-raising (the
    /// re-raise ends the process, so a mark after it would never happen).
    #[test]
    fn the_handler_path_sets_the_flag_before_reraising() {
        use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
        static MARKED: AtomicBool = AtomicBool::new(false);
        static RAISED: AtomicI32 = AtomicI32::new(0);
        static MARKED_AT_RAISE: AtomicBool = AtomicBool::new(false);
        handle(
            libc::SIGTERM,
            || MARKED.store(true, Ordering::SeqCst),
            |sig| {
                MARKED_AT_RAISE.store(MARKED.load(Ordering::SeqCst), Ordering::SeqCst);
                RAISED.store(sig, Ordering::SeqCst);
            },
        );
        assert!(MARKED.load(Ordering::SeqCst));
        assert_eq!(RAISED.load(Ordering::SeqCst), libc::SIGTERM);
        assert!(
            MARKED_AT_RAISE.load(Ordering::SeqCst),
            "mark precedes the re-raise"
        );
    }

    /// Run `child` in a forked process and return its wait status. The child
    /// only makes async-signal-safe calls and always ends with `_exit`, so it
    /// never returns into the test harness.
    fn in_child(child: fn()) -> libc::c_int {
        // SAFETY: fork in a test; the child runs only `child` (sigaction,
        // sigprocmask, kill, an atomic store) and then `_exit`s.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                child();
                libc::_exit(7);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            status
        }
    }

    fn unblock_sigterm() {
        // SAFETY: async-signal-safe mask edit in the forked child.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        }
    }

    fn set_disposition(handler: libc::sighandler_t) {
        // SAFETY: async-signal-safe disposition change in the forked child.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        }
    }

    /// Review round 12 N4: a real SIGTERM still kills the process BY that
    /// signal once the marker is installed (the handler re-raises with the
    /// default action), and an inherited `SIG_IGN` is still honoured.
    #[test]
    fn a_real_sigterm_still_ends_the_process_by_that_signal() {
        let status = in_child(|| {
            unblock_sigterm();
            set_disposition(libc::SIG_DFL);
            super::install_for_test();
            // SAFETY: signalling ourselves.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGTERM);
            }
        });
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(libc::WTERMSIG(status), libc::SIGTERM);

        let status = in_child(|| {
            unblock_sigterm();
            set_disposition(libc::SIG_IGN);
            super::install_for_test();
            // SAFETY: signalling ourselves.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGTERM);
            }
        });
        assert!(
            libc::WIFEXITED(status),
            "ignored SIGTERM: status {status:#x}"
        );
        assert_eq!(libc::WEXITSTATUS(status), 7);
    }

    /// Review round 12 N2: a handler invoked WITHOUT `SA_RESETHAND` — as when
    /// another component (signal-hook) registers later and chains to it — must
    /// still end the process by the signal, not re-raise into itself forever.
    /// A 2 s alarm turns a regression's loop into a SIGALRM death instead of a
    /// hung test.
    #[test]
    fn a_chained_invocation_still_ends_by_the_signal() {
        let status = in_child(|| {
            unblock_sigterm();
            // The handler as a chain would leave it: installed, no RESETHAND.
            set_disposition(super::handler_for_test());
            // SAFETY: a bounded alarm and a signal to ourselves.
            unsafe {
                libc::alarm(2);
                libc::kill(libc::getpid(), libc::SIGTERM);
            }
        });
        assert!(libc::WIFSIGNALED(status), "status {status:#x}");
        assert_eq!(
            libc::WTERMSIG(status),
            libc::SIGTERM,
            "the handler looped until the alarm"
        );
    }
}
