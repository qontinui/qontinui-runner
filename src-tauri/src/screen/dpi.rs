//! Process-wide DPI awareness setup.
//!
//! Modeled on
//! `qontinui/hal/implementations/mss_capture.py::_set_dpi_awareness_once`.
//!
//! On Windows, processes that are not Per-Monitor DPI Aware get the OS to
//! lie about every monitor's resolution, which makes screen captures wrong
//! whenever the user has more than one monitor at different DPIs. We set
//! `PROCESS_PER_MONITOR_DPI_AWARE_V2` once at startup. `E_ACCESSDENIED` is
//! treated as success — Tauri / wry may have already set the awareness
//! level before our call runs, which is fine.
//!
//! On macOS and Linux this is a no-op (the OS handles DPI per-window).

use std::sync::atomic::{AtomicBool, Ordering};

static SET: AtomicBool = AtomicBool::new(false);

/// Call once at process startup, before any screen capture happens.
pub fn ensure_dpi_awareness() {
    if SET.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(target_os = "windows")]
    set_windows_dpi_awareness();
}

#[cfg(target_os = "windows")]
fn set_windows_dpi_awareness() {
    // Declare the Win32 API directly so we don't depend on the
    // `Win32_UI_HiDpi` feature flag of `windows-sys`. This matches the
    // shape used by `qontinui/hal/implementations/mss_capture.py::_set_dpi_awareness_once`
    // which `ctypes`-loads the same function.
    //
    // The DPI_AWARENESS_CONTEXT type is an opaque handle (ptr-sized).
    // PER_MONITOR_AWARE_V2 is the constant `-4 as isize` cast to that
    // handle, per the Windows SDK headers.
    type DpiAwarenessContext = isize;
    const PER_MONITOR_AWARE_V2: DpiAwarenessContext = -4;

    extern "system" {
        fn SetProcessDpiAwarenessContext(value: DpiAwarenessContext) -> i32;
    }

    // SAFETY: `SetProcessDpiAwarenessContext` is callable from any thread,
    // performs no allocation, and the only argument is a documented
    // sentinel value. Returns BOOL: non-zero on success, zero on failure
    // (sets last-error). We ignore the failure path — the most common
    // reason is "already set to a different value", which Tauri / wry may
    // have done before us, and that is fine.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2);
    }
    tracing::debug!("DPI awareness: requested PER_MONITOR_AWARE_V2");
}
