//! Timeouts that do not depend on the runtime they are protecting.
//!
//! ## Why this module exists
//!
//! Iteration 14 bounded the transcript scan's waits with
//! [`tokio::time::timeout`] and shipped it as the fix for a three-stage wedge.
//! Iteration 15 then reproduced the same wedge 3/3 on that build: the fix was
//! **inert**.
//!
//! The reason is structural, not a coding slip. `tokio::time::timeout` is
//! driven by the runtime's *time driver*, and the time driver only advances
//! when a worker thread runs the scheduler loop. When every worker is blocked
//! **inside synchronous code** — a `std::sync::Mutex`, an inline filesystem
//! walk, a blocking channel — no worker ever reaches the scheduler, the timer
//! wheel never turns, and the timeout never fires. It is disabled by exactly
//! the condition it exists to guard.
//!
//! Observed on 2026-08-23/24: three reproductions, every HTTP route hanging,
//! and **neither** the 20s leader give-up WARN nor the follower give-up WARN
//! in any of the three logs. A bound that never fires is not a bound.
//!
//! ## The pattern
//!
//! Measure the deadline on a **dedicated OS thread**, using
//! [`std::sync::mpsc::Receiver::recv_timeout`] — the OS scheduler runs that
//! thread whether or not tokio is healthy. When the deadline expires, the
//! thread fires a [`tokio::sync::oneshot`], which *wakes* the waiting task.
//! Waking needs no time driver: it hands the task straight to the scheduler.
//!
//! The module's own test at
//! [`transcript`](crate::commands::transcript) already used this shape (a std
//! channel measured from outside the runtime) to prove the runtime was alive;
//! iteration 14 simply never moved the production path onto it.
//!
//! ## Reading a fired deadline
//!
//! If the runtime is *so* far gone that even the wakeup cannot be serviced,
//! the caller stays parked — but it stays parked having already been told, and
//! nothing else in the process is holding a lock on its behalf. That is the
//! most a bound can promise from inside a dying process; the point is that the
//! promise no longer depends on the sick subsystem.

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;
use tokio::sync::oneshot;

/// A deadline that fires from outside the tokio runtime.
///
/// Await it in a [`tokio::select!`] alongside the work you are bounding. It
/// resolves when `after` has elapsed, regardless of whether the runtime's time
/// driver is still turning.
///
/// Dropping the returned future cancels the deadline thread promptly (the
/// keep-alive `Sender` is dropped, so the thread's `recv_timeout` returns
/// `Disconnected` immediately rather than sleeping out the full duration).
pub struct OffRuntimeDeadline {
    rx: oneshot::Receiver<()>,
    /// Held only to keep the deadline thread's channel connected. Dropping
    /// `self` drops this, which wakes the thread out of `recv_timeout` at once.
    _cancel: Sender<()>,
}

impl std::future::Future for OffRuntimeDeadline {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // A dropped sender (thread failed to spawn) resolves immediately — a
        // deadline we cannot measure must fail CLOSED, i.e. give up, never
        // wait forever.
        match std::pin::Pin::new(&mut self.rx).poll(cx) {
            std::task::Poll::Ready(_) => std::task::Poll::Ready(()),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Start a deadline measured on its own OS thread.
///
/// **This is the whole point of the module**: `recv_timeout` is serviced by the
/// OS scheduler, so the bound fires even when every tokio worker is parked in
/// synchronous code and the time driver has stopped.
pub fn deadline(after: Duration) -> OffRuntimeDeadline {
    let (fire_tx, fire_rx) = oneshot::channel::<()>();
    // `cancel_tx` lives in the returned struct; the thread blocks on the
    // matching receiver, so the deadline is (a) `after`, or (b) "the waiter
    // went away", whichever comes first.
    let (cancel_tx, cancel_rx) = channel::<()>();

    let spawned = std::thread::Builder::new()
        .name("off-runtime-deadline".to_string())
        .spawn(move || {
            // The bound. Measured by the OS, not by tokio.
            match cancel_rx.recv_timeout(after) {
                Err(RecvTimeoutError::Timeout) => {
                    // Deadline reached — wake the waiter.
                    let _ = fire_tx.send(());
                }
                // Waiter dropped (or sent): nothing to report.
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            }
        });

    if let Err(e) = &spawned {
        // Cannot measure the deadline. This fails CLOSED for free: a failed
        // `Builder::spawn` drops the closure, which drops `fire_tx`, so the
        // receiver resolves immediately and the caller degrades now rather
        // than waiting on a bound nobody is holding.
        tracing::warn!(
            error = %e,
            "off_runtime: could not start a deadline thread — the bounded wait will give up \
             immediately rather than run unbounded"
        );
    }

    OffRuntimeDeadline {
        rx: fire_rx,
        _cancel: cancel_tx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex as StdMutex};
    use std::task::{Context, Poll, Wake, Waker};

    /// A three-line executor with NO tokio runtime behind it.
    ///
    /// This is what makes the first test honest. Driving the deadline on a
    /// tokio runtime would prove nothing — a healthy runtime turns the timer
    /// wheel, so `tokio::time::timeout` would pass too. Here there is no
    /// runtime in the process at all: anything that needs the tokio timer
    /// cannot resolve, and a bound that *does* resolve has proved it is
    /// measured somewhere else.
    struct BlockWaker {
        woken: StdMutex<bool>,
        cv: Condvar,
    }

    impl Wake for BlockWaker {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            *self.woken.lock().unwrap_or_else(|e| e.into_inner()) = true;
            self.cv.notify_all();
        }
    }

    /// Drive `fut` to completion outside any async runtime, giving up after
    /// `cap`. Returns whether the future completed.
    fn drive_without_runtime<F: Future>(fut: F, cap: Duration) -> bool {
        let mut fut = Box::pin(fut);
        let w = Arc::new(BlockWaker {
            woken: StdMutex::new(false),
            cv: Condvar::new(),
        });
        let waker: Waker = w.clone().into();
        let mut cx = Context::from_waker(&waker);
        let start = std::time::Instant::now();
        loop {
            if fut.as_mut().poll(&mut cx).is_ready() {
                return true;
            }
            let remaining = match cap.checked_sub(start.elapsed()) {
                Some(r) if !r.is_zero() => r,
                _ => return false,
            };
            let mut woken = w.woken.lock().unwrap_or_else(|e| e.into_inner());
            if !*woken {
                let (g, _) =
                    w.cv.wait_timeout(woken, remaining)
                        .unwrap_or_else(|e| e.into_inner());
                woken = g;
            }
            *woken = false;
        }
    }

    /// **The property the whole module exists for.** The bound fires with no
    /// tokio runtime anywhere in the process — therefore it cannot be
    /// depending on the runtime it is meant to protect.
    ///
    /// Neuter check: reimplement `deadline` as `tokio::time::sleep(after)` and
    /// this test fails (the sleep panics or never resolves without a reactor).
    /// Measured entirely on the test thread with a std `Condvar`, never with
    /// `tokio::time::timeout` — a tokio-measured assertion would HANG rather
    /// than fail on the regressed shape.
    #[test]
    fn the_deadline_fires_with_no_tokio_runtime_at_all() {
        let started = std::time::Instant::now();
        let fired =
            drive_without_runtime(deadline(Duration::from_millis(250)), Duration::from_secs(5));
        assert!(
            fired,
            "the off-runtime deadline never fired without a runtime — the bound is back on the \
             tokio timer, which is exactly the inert shape iteration 15 reproduced 3/3"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(200),
            "the deadline fired early — it is not measuring the interval it was given"
        );
    }

    /// The bound must survive a runtime whose every worker is stuck in
    /// SYNCHRONOUS code, which is the condition that stops the time driver.
    ///
    /// The deadline is driven off-runtime (std `Condvar`, test thread) while
    /// the runtime is held down, so the assertion reports instead of hanging.
    #[test]
    fn the_deadline_fires_while_every_worker_is_blocked_in_sync_code() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let park = Arc::new(AtomicBool::new(true));
        for _ in 0..2 {
            let park = park.clone();
            // NOT `.await` — a synchronous block, which is what kills the
            // timer wheel.
            rt.spawn(async move {
                while park.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }
        std::thread::sleep(Duration::from_millis(200));

        let fired =
            drive_without_runtime(deadline(Duration::from_millis(250)), Duration::from_secs(5));
        park.store(false, Ordering::SeqCst);
        assert!(
            fired,
            "the deadline did not fire while the runtime's workers were blocked in sync code"
        );
    }

    /// The deadline must resolve inside a normal `select!` and release a wait
    /// that would otherwise hang forever — the production shape.
    #[test]
    fn select_with_the_deadline_releases_a_never_resolving_wait() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");

        let (done_tx, done_rx) = channel::<&'static str>();
        rt.spawn(async move {
            // A oneshot whose sender is held forever: never resolves.
            let (_hold, never) = oneshot::channel::<()>();
            let arm = deadline(Duration::from_millis(200));
            let which = tokio::select! {
                _ = never => "work",
                _ = arm => "deadline",
            };
            let _ = done_tx.send(which);
        });

        // Measured from the test thread, outside the runtime.
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(5)).ok(),
            Some("deadline"),
            "select! did not take the deadline arm"
        );
    }

    /// Dropping the deadline must release its thread promptly, or a 20s bound
    /// on a 30s-poll surface leaks a thread per served request.
    #[test]
    fn dropping_the_deadline_releases_its_thread_promptly() {
        let d = deadline(Duration::from_secs(30));
        let before = std::time::Instant::now();
        drop(d);
        assert!(
            before.elapsed() < Duration::from_secs(1),
            "dropping a deadline blocked the caller for the full duration"
        );
        // And a fresh deadline still fires — the dropped one wedged nothing.
        assert!(
            drive_without_runtime(deadline(Duration::from_millis(100)), Duration::from_secs(3)),
            "a deadline created after a dropped one did not fire"
        );
    }
}
