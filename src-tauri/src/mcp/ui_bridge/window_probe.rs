//! Bounded access to Tauri window geometry getters.
//!
//! # Why this module exists
//!
//! `WebviewWindow::scale_factor()` / `inner_position()` / `inner_size()` /
//! `title()` look like cheap field reads. They are not. Each one posts a
//! message to the **tao event loop** and then blocks the calling thread on
//! `std::sync::mpsc::Receiver::recv()` **with no timeout**
//! (`tauri-runtime-wry` `window_getter!` → `getter!`). `send_user_message`
//! only short-circuits when it is already on the main thread; from any other
//! thread it posts and waits.
//!
//! So when the event loop stops pumping — which is exactly the dead- or
//! hung-webview condition the runner is trying to *diagnose* at that moment —
//! every one of these getters blocks forever.
//!
//! # Why that was categorically worse than an unbounded `spawn_blocking`
//!
//! These were being called directly from `async fn`s served on Tauri's shared
//! multi-threaded runtime (`tauri::async_runtime`, which hosts the whole MCP
//! HTTP API). A *synchronous* block inside an async task **consumes a tokio
//! worker thread**, and that pool is `num_cpus` — not the 512-thread blocking
//! pool. After `num_cpus` concurrent captures against a wedged event loop the
//! runtime has no workers left, and every route, every background task, and
//! the coord-mcp proxy served on this process go silent together.
//!
//! An `.await`ed `spawn_blocking` handle, by contrast, **yields** its worker.
//! That is the same argument the PG pool comment makes about `pool.get()`:
//! parking on a semaphore is fine, blocking a worker is not.
//!
//! # What this module does about it
//!
//! [`geometry`] and [`inner_size`] move the getters onto the blocking pool and
//! wrap them in a timeout. Two things follow, and the second is a real
//! limitation worth stating plainly:
//!
//! 1. The **caller's** worker is never blocked — it awaits a `JoinHandle` and
//!    yields. The runtime keeps serving. This is the property that matters.
//! 2. The blocking-pool thread doing the getter **is still stuck** until the
//!    event loop recovers; the timeout frees the caller, not the thread.
//!    There is no cancellation-safe Tauri getter to call instead, so this is
//!    the best available bound. It trades a `num_cpus`-sized pool for a
//!    512-sized one and converts an unbounded hang into a typed error — it
//!    does not make the underlying getter interruptible.
//!
//! Callers degrade on [`WindowProbeError`] exactly as they already degraded on
//! the `Result`/`Option` these getters return, so a wedged event loop now
//! produces a fast, logged, partial answer instead of a parked worker.

use std::time::Duration;

use tauri::WebviewWindow;

/// How long to wait for the event loop to answer a geometry getter.
///
/// Deliberately shorter than the 5 s `CapturePreview` bound in
/// [`super::screenshots`]: this is a handful of round-trips to a *healthy*
/// event loop (microseconds), so 3 s is already ~6 orders of magnitude of
/// headroom. The only case that reaches the timeout is a genuinely stuck loop,
/// and there the whole point is to give up quickly.
pub const WINDOW_GETTER_TIMEOUT: Duration = Duration::from_secs(3);

/// Why a bounded window getter did not produce a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowProbeError {
    /// The event loop did not answer within [`WINDOW_GETTER_TIMEOUT`].
    ///
    /// This is the diagnostic signal: it means the tao event loop is not
    /// pumping, which is itself strong evidence about *why* the caller was
    /// asked for a diagnostic capture in the first place.
    EventLoopUnresponsive,
    /// The blocking task was cancelled or panicked.
    TaskFailed(String),
}

impl std::fmt::Display for WindowProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventLoopUnresponsive => write!(
                f,
                "window event loop did not respond within {:?} — the UI thread is wedged",
                WINDOW_GETTER_TIMEOUT
            ),
            Self::TaskFailed(e) => write!(f, "window geometry probe task failed: {e}"),
        }
    }
}

/// Everything the capture paths need from a window, fetched in one bounded
/// hop.
///
/// Batched deliberately: each field is a separate event-loop round-trip, so
/// fetching them together behind ONE timeout means a wedged loop costs one
/// wait instead of four sequential ones.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowGeometry {
    pub scale: f64,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub title: String,
}

/// Fetch the full geometry set under a single [`WINDOW_GETTER_TIMEOUT`].
///
/// Per-field fallbacks (`unwrap_or`) are preserved from the original call
/// sites — an individual getter returning `Err` is a different, benign
/// condition from the event loop being wedged, and only the latter produces
/// [`WindowProbeError`].
pub async fn geometry(window: &WebviewWindow) -> Result<WindowGeometry, WindowProbeError> {
    let window = window.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let scale = window.scale_factor().unwrap_or(1.0);
        let pos = window.inner_position().unwrap_or_default();
        let size = window.inner_size().unwrap_or_default();
        let title = window
            .title()
            .unwrap_or_else(|_| "Qontinui Runner".to_string());
        WindowGeometry {
            scale,
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
            title,
        }
    });

    match tokio::time::timeout(WINDOW_GETTER_TIMEOUT, handle).await {
        Ok(Ok(geometry)) => Ok(geometry),
        Ok(Err(join_err)) => Err(WindowProbeError::TaskFailed(join_err.to_string())),
        Err(_elapsed) => Err(WindowProbeError::EventLoopUnresponsive),
    }
}

/// Bounded `inner_size()` for callers that need only the physical size.
///
/// Returns `Ok(None)` when the getter itself failed but the event loop DID
/// answer — the same "no size available, carry on" signal the bare
/// `.ok()` produced before, kept distinct from a wedged loop.
pub async fn inner_size(
    window: &WebviewWindow,
) -> Result<Option<tauri::PhysicalSize<u32>>, WindowProbeError> {
    let window = window.clone();
    let handle = tokio::task::spawn_blocking(move || window.inner_size().ok());

    match tokio::time::timeout(WINDOW_GETTER_TIMEOUT, handle).await {
        Ok(Ok(size)) => Ok(size),
        Ok(Err(join_err)) => Err(WindowProbeError::TaskFailed(join_err.to_string())),
        Err(_elapsed) => Err(WindowProbeError::EventLoopUnresponsive),
    }
}

/// Bounded `scale_factor()`, for callers that need only the scale.
pub async fn scale_factor(window: &WebviewWindow) -> Result<f64, WindowProbeError> {
    let window = window.clone();
    let handle = tokio::task::spawn_blocking(move || window.scale_factor().unwrap_or(1.0));

    match tokio::time::timeout(WINDOW_GETTER_TIMEOUT, handle).await {
        Ok(Ok(scale)) => Ok(scale),
        Ok(Err(join_err)) => Err(WindowProbeError::TaskFailed(join_err.to_string())),
        Err(_elapsed) => Err(WindowProbeError::EventLoopUnresponsive),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The timeout must stay well under the caller-visible budgets that wrap
    /// it — the 5 s `CapturePreview` bound and the supervisor's health probe.
    /// If someone raises this to "be safe", the bound stops bounding.
    #[test]
    fn getter_timeout_is_shorter_than_the_capture_preview_bound() {
        assert!(
            WINDOW_GETTER_TIMEOUT < Duration::from_secs(5),
            "WINDOW_GETTER_TIMEOUT must stay below the 5s CapturePreview bound, \
             or a wedged event loop outlives the capture timeout that exists to \
             contain it"
        );
    }

    #[test]
    fn unresponsive_error_names_the_wedge_not_a_generic_timeout() {
        // The message is load-bearing: it is what tells an operator reading
        // the log that the UI thread is the problem, rather than the capture.
        let msg = WindowProbeError::EventLoopUnresponsive.to_string();
        assert!(msg.contains("wedged"), "unexpected message: {msg}");
        assert!(msg.contains("event loop"), "unexpected message: {msg}");
    }

    #[test]
    fn task_failure_is_distinct_from_an_unresponsive_loop() {
        // Callers branch on this: a join failure is a bug in us, an
        // unresponsive loop is a diagnosis of the webview.
        assert_ne!(
            WindowProbeError::TaskFailed("boom".into()),
            WindowProbeError::EventLoopUnresponsive
        );
    }
}
