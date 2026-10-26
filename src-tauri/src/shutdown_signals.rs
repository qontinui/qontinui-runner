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
        // SAFETY: `SA_RESETHAND` already restored `SIG_DFL`; re-raising the
        // signal (blocked while this handler runs) delivers the default
        // action as the handler returns — the process ends exactly as it did
        // with no handler.
        unsafe {
            libc::raise(sig);
        }
    }

    extern "C" fn on_signal(sig: libc::c_int) {
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
}
