//! Generic task supervisor for long-lived `tokio` loops.
//!
//! Several runner subsystems own a single long-lived task whose loop is only
//! supposed to RETURN when a shutdown is signalled (the backend WS relay, the
//! device-JWT refresher). Spawned bare (`tokio::spawn(loop_fut)`), a single
//! panic anywhere in the loop's directly-awaited path unwinds the task and
//! PERMANENTLY kills the subsystem — the stored `JoinHandle` is never observed,
//! so any `watch`-based kick sent afterwards lands on a dropped receiver
//! (a silent no-op) and only a full runner restart can revive it. That is the
//! exact failure mode that left the relay `ws_connected:false` with every
//! idle-gate green and kicks doing nothing (RCA 2026-06-05/06).
//!
//! [`spawn_supervised`] wraps such a loop in a supervisor that respawns it on a
//! panic OR an unexpected (non-shutdown) return, with escalating backoff so a
//! panic that reproduces immediately can't hot-spin. `catch_unwind` keeps a
//! loop panic from escaping the supervisor (the crate builds `panic=unwind`;
//! sentry still records the panic via its panic integration). A deliberate
//! shutdown signal is the only clean exit.
//!
//! This is the ONE task-supervision idiom for these `watch`-driven loops — both
//! the relay and the refresher build on it rather than carrying divergent
//! bespoke JoinHandle-respawn helpers.

use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

/// Base backoff before the first respawn after a death.
const INITIAL_RESPAWN_BACKOFF: Duration = Duration::from_secs(1);
/// Cap on the respawn backoff so repeated quick deaths back off but never
/// stall recovery for too long.
const MAX_RESPAWN_BACKOFF: Duration = Duration::from_secs(30);
/// A run that stayed alive at least this long is considered "healthy" and
/// resets the backoff — so a one-off death after a long healthy run respawns
/// promptly, while a tight crash-loop escalates.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// Spawn `make_run`'s future under a respawn supervisor and return the
/// supervisor's `JoinHandle`.
///
/// `make_run` is a factory invoked once per (re)spawn — it builds a fresh loop
/// future. Capture any `watch::Receiver`s it needs (e.g. a kick receiver) in
/// the closure and clone a fresh one inside it; because the closure outlives
/// every respawn, those receivers SURVIVE a death, so a kick sent after a
/// respawn still reaches the new loop (the bug the bare-spawn pattern had).
///
/// The loop is expected to return ONLY when `shutdown_rx` is set to `true`;
/// any other return (or panic) is treated as an unexpected death and respawned.
pub(crate) fn spawn_supervised<F, Fut>(
    name: &'static str,
    shutdown_rx: watch::Receiver<bool>,
    make_run: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(supervise_loop(
        name,
        shutdown_rx,
        INITIAL_RESPAWN_BACKOFF,
        MAX_RESPAWN_BACKOFF,
        HEALTHY_RUN,
        make_run,
    ))
}

/// The supervisor body. Kept as a standalone `async fn` (rather than inline in
/// [`spawn_supervised`]) with the backoff windows as parameters so it is
/// directly unit-testable with a panic-on-cue loop and millisecond backoffs
/// (the repo's `tokio` has `full` but not `test-util`, so paused-clock tests
/// aren't available — see `util::path_extraction` tests for the same note).
async fn supervise_loop<F, Fut>(
    name: &'static str,
    mut shutdown_rx: watch::Receiver<bool>,
    initial_backoff: Duration,
    max_backoff: Duration,
    healthy_run: Duration,
    mut make_run: F,
) where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    let mut respawn_backoff = initial_backoff;

    loop {
        if *shutdown_rx.borrow() {
            info!("{name} supervisor: shutdown observed, exiting");
            return;
        }

        let started = std::time::Instant::now();
        // `AssertUnwindSafe`: the loop owns its own state; a panic leaves no
        // shared invariant broken across respawns (a fresh future is built
        // each time), so catching the unwind here is sound.
        let run = AssertUnwindSafe(make_run()).catch_unwind().await;

        // A shutdown is the ONLY clean reason for the loop to return.
        if *shutdown_rx.borrow() {
            info!("{name} supervisor: loop exited on shutdown, exiting");
            return;
        }

        match run {
            Ok(()) => warn!(
                "{name} loop returned without a shutdown signal after {:.1}s — respawning",
                started.elapsed().as_secs_f64()
            ),
            Err(_panic) => warn!(
                "{name} loop PANICKED after {:.1}s — respawning (it would otherwise stay dead \
                 until a runner restart)",
                started.elapsed().as_secs_f64()
            ),
        }

        // A loop that ran healthy for a while before dying resets the backoff;
        // a quick re-death escalates it (capped).
        if started.elapsed() >= healthy_run {
            respawn_backoff = initial_backoff;
        }
        // Sleep before respawning, but wake immediately on shutdown so a stop()
        // during the backoff window isn't delayed.
        tokio::select! {
            _ = tokio::time::sleep(respawn_backoff) => {}
            _ = shutdown_rx.changed() => {
                info!("{name} supervisor: shutdown during respawn backoff, exiting");
                return;
            }
        }
        respawn_backoff = (respawn_backoff * 2).min(max_backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;

    // Millisecond backoffs so the tests run in real time but finish fast. The
    // repo's `tokio` has `full` but not `test-util`, so `start_paused` isn't
    // available; driving `supervise_loop` directly with tiny windows is the
    // deterministic alternative. `healthy_run = 0` keeps the backoff pinned at
    // `initial` (every run counts as healthy) so timing stays negligible.
    const FAST_INITIAL: Duration = Duration::from_millis(1);
    const FAST_MAX: Duration = Duration::from_millis(5);
    const FAST_HEALTHY: Duration = Duration::from_millis(0);

    /// The core regression anchor: a loop that PANICS on cue is respawned each
    /// time, and the supervisor exits cleanly once shutdown is signalled.
    #[tokio::test]
    async fn respawns_after_panic_until_shutdown() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let runs = Arc::new(AtomicUsize::new(0));
        let runs2 = runs.clone();
        let shutdown_tx2 = shutdown_tx.clone();

        let supervisor = supervise_loop(
            "test-panic",
            shutdown_rx,
            FAST_INITIAL,
            FAST_MAX,
            FAST_HEALTHY,
            move || {
                let runs = runs2.clone();
                let shutdown_tx = shutdown_tx2.clone();
                async move {
                    let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                    if n >= 4 {
                        // Survived enough respawns; ask the supervisor to stop.
                        let _ = shutdown_tx.send(true);
                        return;
                    }
                    panic!("boom #{n}");
                }
            },
        );

        tokio::time::timeout(Duration::from_secs(10), supervisor)
            .await
            .expect("supervisor should exit after shutdown");

        // 3 panics + 1 clean run that signals shutdown.
        assert_eq!(runs.load(Ordering::SeqCst), 4);
    }

    /// A clean (non-panic) return that is NOT a shutdown is also treated as an
    /// unexpected death and respawned — the loop is supposed to run forever
    /// until shutdown.
    #[tokio::test]
    async fn respawns_on_unexpected_clean_return() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let runs = Arc::new(AtomicUsize::new(0));
        let runs2 = runs.clone();
        let shutdown_tx2 = shutdown_tx.clone();

        let supervisor = supervise_loop(
            "test-clean-return",
            shutdown_rx,
            FAST_INITIAL,
            FAST_MAX,
            FAST_HEALTHY,
            move || {
                let runs = runs2.clone();
                let shutdown_tx = shutdown_tx2.clone();
                async move {
                    let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                    if n >= 3 {
                        let _ = shutdown_tx.send(true);
                    }
                    // Always returns cleanly — without shutdown this must respawn.
                }
            },
        );

        tokio::time::timeout(Duration::from_secs(10), supervisor)
            .await
            .expect("supervisor should exit after shutdown");

        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    /// A kick receiver captured by the factory SURVIVES respawns: every run —
    /// including those after a panic-respawn — can still read the kick channel.
    /// This is the property the bare-spawn pattern lost (the receiver dropped
    /// with the dead task, turning every later kick into a silent no-op).
    #[tokio::test]
    async fn kick_receiver_survives_respawn() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (kick_tx, kick_rx) = watch::channel(0u64);
        kick_tx.send(7).expect("seed kick value");

        let runs = Arc::new(AtomicUsize::new(0));
        let all_reads_ok = Arc::new(AtomicBool::new(true));
        let last_seen = Arc::new(AtomicU64::new(0));
        let runs2 = runs.clone();
        let all_reads_ok2 = all_reads_ok.clone();
        let last_seen2 = last_seen.clone();
        let shutdown_tx2 = shutdown_tx.clone();

        let supervisor = supervise_loop(
            "test-kick",
            shutdown_rx,
            FAST_INITIAL,
            FAST_MAX,
            FAST_HEALTHY,
            move || {
                let runs = runs2.clone();
                let all_reads_ok = all_reads_ok2.clone();
                let last_seen = last_seen2.clone();
                let shutdown_tx = shutdown_tx2.clone();
                // The factory OWNS the kick receiver across all respawns.
                let mut kr = kick_rx.clone();
                async move {
                    // Reading the kick channel on a respawned run must work.
                    let v = *kr.borrow_and_update();
                    last_seen.store(v, Ordering::SeqCst);
                    if v != 7 {
                        all_reads_ok.store(false, Ordering::SeqCst);
                    }
                    let n = runs.fetch_add(1, Ordering::SeqCst) + 1;
                    if n >= 3 {
                        let _ = shutdown_tx.send(true);
                        return;
                    }
                    panic!("boom — forcing a respawn");
                }
            },
        );
        // Keep the sender alive for the whole run so the receiver stays live.
        let _kick_tx = kick_tx;

        tokio::time::timeout(Duration::from_secs(10), supervisor)
            .await
            .expect("supervisor should exit after shutdown");

        assert!(
            all_reads_ok.load(Ordering::SeqCst),
            "kick value should be readable on every respawned run"
        );
        assert_eq!(last_seen.load(Ordering::SeqCst), 7);
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    /// A shutdown signalled while the loop is dying repeatedly is observed and
    /// the supervisor exits promptly (doesn't spin forever).
    #[tokio::test]
    async fn shutdown_stops_a_crash_looping_task() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let runs = Arc::new(AtomicUsize::new(0));
        let runs2 = runs.clone();

        // Pre-signal shutdown so the supervisor exits at its first pre-run
        // check or its first backoff select rather than crash-looping forever.
        let _ = shutdown_tx.send(true);

        let supervisor = supervise_loop(
            "test-shutdown",
            shutdown_rx,
            FAST_INITIAL,
            FAST_MAX,
            FAST_HEALTHY,
            move || {
                let runs = runs2.clone();
                async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    panic!("die");
                }
            },
        );

        tokio::time::timeout(Duration::from_secs(10), supervisor)
            .await
            .expect("supervisor should exit promptly on shutdown");

        // Shutdown was set before the first run, so the supervisor exits at the
        // pre-run check without ever invoking the loop.
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
