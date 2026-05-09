//! Centralized window-placement strategy.
//!
//! All paths that create the runner's main window — primary, named
//! instance, supervisor-spawned temp — funnel through this module. The
//! goal is one source of truth for "where does the window go and how do
//! we make it actually land there".
//!
//! The hard part is multi-monitor + mixed-DPI placement on Windows:
//!
//! * `WebviewWindowBuilder::position(x, y)` is logical pixels and is
//!   applied while the window is still being created on whichever
//!   monitor `.build()` happens to land on (typically the user-active
//!   monitor, with whatever DPI it has).
//! * `WebviewWindow::set_position(PhysicalPosition)` *should* be
//!   absolute virtual-desktop physical pixels but in practice the move
//!   is done in the source monitor's DPI context, so the very first
//!   call gets re-scaled when the window crosses a DPI boundary. The
//!   second call — fired after the move — runs in the target monitor's
//!   DPI context and lands correctly.
//! * Win10/11 reserves a ~7-8px invisible "extended frame" around every
//!   top-level window for the drop shadow, even with `decorations(false)`.
//!   The maximize path special-cases this (Windows internally shifts
//!   the outer rect so the visible client area fills the work area),
//!   which is why the primary runner has always looked flush. Explicit
//!   placements don't get that for free — we compensate post-build by
//!   reading `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` and
//!   shifting the outer rect by the inset so the *visible* edge lands
//!   exactly on the configured X/Y.

use serde::Serialize;
use tracing::{info, warn};

/// What we want done to the main window after `WebviewWindowBuilder::build()`.
#[derive(Debug, Clone, Serialize)]
pub enum WindowPlacement {
    /// 100,100 fallback for secondary instances without configured placement.
    SecondaryDefault,
    /// Maximize on the active / primary monitor.
    Maximized,
    /// Place at exact physical rect on the virtual desktop.
    Positioned { x: i32, y: i32, w: u32, h: u32 },
}

impl WindowPlacement {
    /// Pre-build configuration applied to the builder.
    ///
    /// Note: for `Positioned`, we deliberately do NOT call `.position()`
    /// or `.inner_size()` here. tao's builder converts those values
    /// through `Position::to_physical(self.scale_factor())` using the
    /// current source-monitor scale, which mangles cross-monitor
    /// placements when the source and target monitors have different
    /// DPI. Instead we let the window land at OS default and reposition
    /// it post-build via raw `SetWindowPos` (see `finalize`).
    pub fn configure_builder<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
        &self,
        builder: tauri::WebviewWindowBuilder<'a, R, M>,
    ) -> tauri::WebviewWindowBuilder<'a, R, M> {
        match self {
            Self::SecondaryDefault => builder.position(100.0, 100.0),
            Self::Maximized => builder.maximized(true),
            Self::Positioned { .. } => builder,
        }
    }

    /// Post-build finalization.
    ///
    /// `Positioned` requires extra work to actually land where we asked
    /// (see module docs). Other variants are no-ops here.
    pub fn finalize<R: tauri::Runtime>(&self, win: &tauri::WebviewWindow<R>) {
        let Self::Positioned { x, y, w, h } = self else {
            return;
        };

        #[cfg(windows)]
        {
            // We bypass `WebviewWindow::set_position` / `set_size` on
            // Windows because tao's `set_outer_position` runs the value
            // through `Position::to_physical(self.scale_factor())`, and
            // `self.scale_factor()` is the *source* monitor's scale at
            // the moment of the call — not the target's. Even when we
            // pass `PhysicalPosition`, `Position` is enum-wrapped and
            // tao's path inspects the current window context, leading to
            // off-by-monitor-DPI placements when the window crosses a
            // DPI boundary at startup.
            //
            // Calling `SetWindowPos` directly with virtual-desktop
            // physical coords sidesteps the conversion entirely
            // (per-monitor-v2 DPI awareness makes those coords absolute).
            // We then read `DWMWA_EXTENDED_FRAME_BOUNDS` and shift by the
            // invisible-shadow inset so the *visible* edge lands flush
            // with the configured X/Y — same effect Windows gets for free
            // when a window is maximized.
            apply_via_setwindowpos(win, *x, *y, *w, *h);
            return;
        }

        // Non-Windows fallback: keep using the Tauri API.
        #[cfg(not(windows))]
        {
            let pos = tauri::PhysicalPosition::new(*x, *y);
            let size = tauri::PhysicalSize::new(*w, *h);
            if let Err(e) = win.set_position(pos) {
                warn!("placement: set_position failed: {}", e);
            }
            if let Err(e) = win.set_size(size) {
                warn!("placement: set_size failed: {}", e);
            }
        }
    }
}

#[cfg(windows)]
fn apply_via_setwindowpos<R: tauri::Runtime>(
    win: &tauri::WebviewWindow<R>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::HiDpi::{
        GetThreadDpiAwarenessContext, SetThreadDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
    };

    let tauri_hwnd = match win.hwnd() {
        Ok(h) => h,
        Err(e) => {
            warn!(
                "placement: hwnd() failed, falling back to set_position: {}",
                e
            );
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
            let _ = win.set_size(tauri::PhysicalSize::new(w, h));
            return;
        }
    };
    let hwnd: HWND = tauri_hwnd.0 as HWND;

    // Force per-monitor-v2 DPI context for this thread for the duration
    // of our SetWindowPos calls. With v2, the X/Y/W/H args are absolute
    // virtual-desktop physical pixels — no implicit scaling. Without
    // this, a thread inheriting a different awareness mode (e.g. system
    // DPI aware) would have the OS divide our coords by the system DPI
    // scale, sending the window to the wrong monitor.
    let prev_ctx =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let _restore_guard = ThreadDpiContextGuard {
        prev: prev_ctx,
        active: !prev_ctx.is_null(),
    };
    if prev_ctx.is_null() {
        warn!("placement: SetThreadDpiAwarenessContext returned NULL (Win10 1607+ required)");
    } else {
        let current = unsafe { GetThreadDpiAwarenessContext() };
        info!(
            "placement: thread DPI context now {:?} (was {:?})",
            current, prev_ctx
        );
    }

    // First pass: place the outer rect at exactly (x, y) with size (w, h).
    // SWP_NOACTIVATE keeps focus where it was; we'll show/focus afterward.
    let result = unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            x,
            y,
            w as i32,
            h as i32,
            SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
        )
    };
    if result == 0 {
        warn!("placement: SetWindowPos (1st pass) failed");
    }

    // Compensate for the DWM extended-frame (invisible drop-shadow ring).
    // We measure it AFTER the move so the values reflect the target
    // monitor's DWM state.
    let (inset_x, inset_y) = windows_frame_inset(win);
    if inset_x != 0 || inset_y != 0 {
        let result = unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x - inset_x,
                y - inset_y,
                w as i32 + 2 * inset_x,
                h as i32 + 2 * inset_y,
                SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
            )
        };
        if result == 0 {
            warn!("placement: SetWindowPos (DWM compensation) failed");
        } else {
            info!(
                "placement: compensated DWM frame inset by ({}, {}); outer rect → ({}, {}) {}x{}",
                inset_x,
                inset_y,
                x - inset_x,
                y - inset_y,
                w as i32 + 2 * inset_x,
                h as i32 + 2 * inset_y,
            );
        }
    } else {
        info!(
            "placement: outer rect at ({}, {}) {}x{} (no DWM inset detected)",
            x, y, w, h
        );
    }
}

/// RAII guard that restores the previous thread DPI awareness context
/// when dropped. Avoids leaking the per-monitor-v2 override into other
/// Tauri/tao code that may rely on the inherited context.
#[cfg(windows)]
struct ThreadDpiContextGuard {
    prev: windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT,
    active: bool,
}

#[cfg(windows)]
impl Drop for ThreadDpiContextGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                windows_sys::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(self.prev);
            }
        }
    }
}

/// On Windows, return `(visible_left - outer_left, visible_top - outer_top)`
/// — the offset from the OS-reported outer window rect to the visible
/// content edge. Always non-negative in practice; `(0, 0)` on failure.
#[cfg(windows)]
fn windows_frame_inset<R: tauri::Runtime>(win: &tauri::WebviewWindow<R>) -> (i32, i32) {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

    // Tauri's `hwnd()` returns its own crate's `HWND` newtype around
    // `*mut c_void`. Convert via raw pointer for FFI compatibility with
    // windows-sys' raw `HWND = *mut c_void`.
    let tauri_hwnd = match win.hwnd() {
        Ok(h) => h,
        Err(_) => return (0, 0),
    };
    let hwnd: HWND = tauri_hwnd.0 as HWND;

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
