//! Win32 compatibility / glue helpers.
//!
//! Consolidates the small set of Win32 patterns that other modules
//! (`window_placement`, future capture / window sites) repeatedly need:
//!
//! * Bridging Tauri's `windows`-crate `HWND` newtype across to the raw
//!   `windows-sys` `HWND` we use everywhere else in the runner. Tauri 2.x
//!   returns its own crate's `HWND` from `WebviewWindow::hwnd()`; the rest
//!   of our Win32 calls go through `windows-sys` 0.59, whose `HWND` is a
//!   plain `*mut c_void`. Both are pointer-sized so a single
//!   `tauri_hwnd.0 as windows_sys::HWND` cast bridges them — but doing
//!   that cast at every call site is noise, easy to typo, and tedious to
//!   audit. [`hwnd_from_tauri`] is the one place that does it.
//!
//! * Per-thread DPI awareness. Several Win32 APIs (notably `SetWindowPos`
//!   for cross-monitor placement, and any future `MapWindowPoints` /
//!   `LogicalToPhysicalPointForPerMonitorDPI` callers) require the
//!   calling thread to be in `PER_MONITOR_AWARE_V2` so coordinate args
//!   are taken as absolute virtual-desktop physical pixels rather than
//!   silently divided by the system DPI. [`ThreadDpiContextGuard`] is
//!   the RAII handle (set on construction, restore on drop) and
//!   [`with_v2_dpi`] is the convenience wrapper for the common
//!   "set-and-call-closure-and-restore" shape — so adding a new
//!   Win32-touching feature stops being "copy the guard pattern from
//!   `window_placement.rs`".
//!
//! * DWM extended-frame inset. Win10/11 reserves a ~7-8px invisible
//!   ring around every top-level window for the drop shadow, even when
//!   `decorations(false)`. Explicit placements need to compensate for it
//!   so the *visible* edge lands flush with the configured X/Y.
//!   [`frame_inset`] reads `DWMWA_EXTENDED_FRAME_BOUNDS` and returns the
//!   `(left, top)` inset between the OS-reported outer rect and the
//!   visible content edge. Always non-negative in practice; `(0, 0)` on
//!   any failure (DWM disabled, hwnd invalid, etc.).
//!
//! All Win32 entry points in this module are gated by
//! `#[cfg(target_os = "windows")]`. A non-Windows stub is provided for
//! [`with_v2_dpi`] so the rest of the codebase can call it without
//! sprinkling `#[cfg]` everywhere.

#[cfg(target_os = "windows")]
pub use platform::*;

#[cfg(not(target_os = "windows"))]
pub use stub::*;

#[cfg(target_os = "windows")]
mod platform {
    use tracing::warn;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    /// Bridge a Tauri `WebviewWindow`'s HWND (which comes from Tauri's
    /// internal `windows` crate, a different version from our
    /// `windows-sys` 0.59) over to the `windows-sys` HWND used by the
    /// rest of the runner.
    ///
    /// Returns `None` if Tauri can't produce a handle (e.g. window has
    /// been destroyed) or the platform back-end isn't Windows.
    pub fn hwnd_from_tauri<R: tauri::Runtime>(win: &tauri::WebviewWindow<R>) -> Option<HWND> {
        match win.hwnd() {
            Ok(h) => Some(h.0 as HWND),
            Err(_) => None,
        }
    }

    /// RAII guard that restores the previous thread DPI awareness
    /// context when dropped.
    ///
    /// Avoids leaking a per-monitor-v2 override into other Tauri / tao
    /// code that may be relying on the inherited context. Construct via
    /// [`ThreadDpiContextGuard::set_per_monitor_v2`] for the most common
    /// case, or directly when you need to restore something other than
    /// the value present at the moment of `set_*`.
    pub struct ThreadDpiContextGuard {
        prev: DPI_AWARENESS_CONTEXT,
        active: bool,
    }

    impl ThreadDpiContextGuard {
        /// Switch the calling thread to `PER_MONITOR_AWARE_V2` and return
        /// a guard that restores the previous context on drop.
        ///
        /// On success the returned guard's `active` flag is `true`. If
        /// `SetThreadDpiAwarenessContext` returns NULL — Win10 1607+ is
        /// required for the v2 context — the guard's `active` flag is
        /// `false` and drop becomes a no-op. The caller can still use
        /// [`ThreadDpiContextGuard::is_active`] to detect this and log
        /// or fall back as needed.
        pub fn set_per_monitor_v2() -> Self {
            let prev =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            Self {
                prev,
                active: !prev.is_null(),
            }
        }

        /// Whether the guard is actively holding a DPI context override.
        /// `false` means `SetThreadDpiAwarenessContext` returned NULL on
        /// construction; `true` means drop will restore `prev`.
        pub fn is_active(&self) -> bool {
            self.active
        }
    }

    impl Drop for ThreadDpiContextGuard {
        fn drop(&mut self) {
            if self.active {
                unsafe {
                    SetThreadDpiAwarenessContext(self.prev);
                }
            }
        }
    }

    /// Run `f` with the calling thread set to `PER_MONITOR_AWARE_V2`,
    /// restoring the previous DPI context when `f` returns.
    ///
    /// The convenience wrapper for the common
    /// "set-DPI-then-call-Win32-then-restore" pattern. If
    /// `SetThreadDpiAwarenessContext` returns NULL (i.e. Win10 < 1607)
    /// the closure still runs — without the v2 override — and a warning
    /// is logged. This matches the existing behavior in
    /// `window_placement::apply_via_setwindowpos`.
    pub fn with_v2_dpi<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let guard = ThreadDpiContextGuard::set_per_monitor_v2();
        if !guard.is_active() {
            warn!("win32_compat: SetThreadDpiAwarenessContext returned NULL (Win10 1607+ required); proceeding without v2 override");
        }
        f()
    }

    /// Return `(visible_left - outer_left, visible_top - outer_top)` —
    /// the DWM extended-frame inset between the OS-reported outer
    /// window rect and the visible content edge. Always non-negative
    /// in practice; `(0, 0)` on any failure.
    ///
    /// Wraps `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` and
    /// `GetWindowRect`. The inset reflects the target monitor's DWM
    /// state at the moment of the call, so callers placing windows
    /// across DPI / theme boundaries should call this *after* the move
    /// (or after a `SetWindowPos` no-op refresh) to get accurate values.
    pub fn frame_inset(hwnd: HWND) -> (i32, i32) {
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::Graphics::Dwm::{
            DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

        if hwnd.is_null() {
            return (0, 0);
        }

        unsafe {
            let mut frame_bounds: RECT = std::mem::zeroed();
            let hr = DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut frame_bounds as *mut RECT as *mut std::ffi::c_void,
                std::mem::size_of::<RECT>() as u32,
            );
            if hr != 0 {
                return (0, 0);
            }

            let mut window_rect: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut window_rect) == 0 {
                return (0, 0);
            }

            (
                frame_bounds.left - window_rect.left,
                frame_bounds.top - window_rect.top,
            )
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod stub {
    /// Non-Windows stub for [`super::with_v2_dpi`]. Just runs the closure.
    ///
    /// Provided so callers don't need to wrap every use-site in
    /// `#[cfg(target_os = "windows")]`.
    pub fn with_v2_dpi<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        f()
    }
}
