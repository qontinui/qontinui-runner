//! Window state on Windows: foreground, cloaked (`DWMWA_CLOAKED`), minimized,
//! occluded, not responding (`IsHungAppWindow`); target lookup; the display's
//! refresh interval; and making a window foreground.
//!
//! Occluders follow the rules Chromium's native window occlusion tracker uses
//! to decide that a window covers another (`IsWindowVisibleAndFullyOpaque`):
//! visible, not minimized, not cloaked, not click-through (`WS_EX_TRANSPARENT`),
//! not a tool window (except the taskbar), not a translucent or colour-keyed
//! layered window, not a non-rectangular region, not a transient popup (except
//! the taskbar), non-empty. D2's concern is exactly Chromium deciding a window
//! is covered and stopping drawing, so the probe uses the same definition.

use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmGetCompositionTimingInfo, DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
    DWM_TIMING_INFO,
};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, DeleteObject, EnumDisplaySettingsW, GetMonitorInfoW, GetWindowRgn,
    MonitorFromWindow, COMPLEXREGION, DEVMODEW, ENUM_CURRENT_SETTINGS, HGDIOBJ, MONITORINFOEXW,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetLayeredWindowAttributes, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible,
    SetForegroundWindow, ShowWindow, SwitchToThisWindow, GA_ROOT, GWL_EXSTYLE, GWL_STYLE,
    GW_HWNDPREV, GW_OWNER, LAYERED_WINDOW_ATTRIBUTES_FLAGS, LWA_ALPHA, LWA_COLORKEY, SW_RESTORE,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};

use super::{handle_of, hwnd};
use crate::clock::qpc_ticks_to_ns;
use crate::model::WindowSample;
use crate::platform::common::occlusion;
use crate::platform::{Refresh, TargetSpec, WindowHandle, WindowInspector};
use crate::raster::Rect;
use crate::records::OccluderInfo;

fn rect_of(r: RECT) -> Rect {
    Rect {
        x: r.left,
        y: r.top,
        w: (r.right - r.left).max(0) as u32,
        h: (r.bottom - r.top).max(0) as u32,
    }
}

/// The window's visible bounds in physical screen pixels (the extended frame
/// bounds, which exclude Windows 10's invisible resize borders), falling back
/// to the window rectangle.
pub fn bounds(h: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    // SAFETY: DwmGetWindowAttribute writes at most size_of::<RECT>() bytes.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut core::ffi::c_void,
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        // SAFETY: writes one RECT.
        unsafe { GetWindowRect(h, &mut r) }.ok()?;
    }
    let rect = rect_of(r);
    (rect.area() > 0).then_some(rect)
}

/// `DWMWA_CLOAKED` (another virtual desktop, a suspended app, ...).
pub fn cloaked(h: HWND) -> Option<bool> {
    let mut v: u32 = 0;
    // SAFETY: writes one u32.
    unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut v as *mut u32 as *mut core::ffi::c_void,
            4,
        )
        .ok()?;
    }
    Some(v != 0)
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer length is passed by the slice.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn title(h: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer length is passed by the slice.
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: writes one u32.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Chromium's opaque-occluder rules; `Some(bounds)` when `w` covers what is
/// beneath it.
fn occluder_bounds(w: HWND) -> Option<Rect> {
    // SAFETY: every call here only reads window properties of a handle that
    // may have died meanwhile, which each call tolerates (failure = skip).
    unsafe {
        if !IsWindowVisible(w).as_bool() || IsIconic(w).as_bool() {
            return None;
        }
        let ex = GetWindowLongW(w, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TRANSPARENT.0 != 0 {
            return None;
        }
        let taskbar = || class_name(w) == "Shell_TrayWnd";
        if ex & WS_EX_TOOLWINDOW.0 != 0 && !taskbar() {
            return None;
        }
        if ex & WS_EX_LAYERED.0 != 0 {
            let mut alpha = 0u8;
            let mut flags = LAYERED_WINDOW_ATTRIBUTES_FLAGS(0);
            let mut key = COLORREF(0);
            // UpdateLayeredWindow windows fail this call: treated as
            // transparent, as Chromium does.
            GetLayeredWindowAttributes(w, Some(&mut key), Some(&mut alpha), Some(&mut flags))
                .ok()?;
            if (flags.0 & LWA_ALPHA.0 != 0 && alpha < 255) || flags.0 & LWA_COLORKEY.0 != 0 {
                return None;
            }
        }
        let rgn = CreateRectRgn(0, 0, 0, 0);
        let complex = GetWindowRgn(w, rgn) == COMPLEXREGION;
        let _ = DeleteObject(HGDIOBJ(rgn.0));
        if complex {
            return None;
        }
        if cloaked(w) == Some(true) {
            return None;
        }
        let style = GetWindowLongW(w, GWL_STYLE) as u32;
        if style & WS_POPUP.0 != 0 && !taskbar() {
            return None;
        }
    }
    bounds(w)
}

/// Is `target` (a top-level window) the foreground window's root?
pub fn is_foreground(target: HWND) -> bool {
    // SAFETY: reads the foreground handle and its root.
    unsafe {
        let fg = GetForegroundWindow();
        !fg.is_invalid() && GetAncestor(fg, GA_ROOT) == target
    }
}

/// Describe the current foreground window (class and pid; never a title).
pub fn foreground_description() -> String {
    // SAFETY: reads the foreground handle.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        "no foreground window".into()
    } else {
        format!("{} (pid {})", class_name(fg), pid_of(fg))
    }
}

/// Samples one window.
pub struct Inspector {
    target: isize,
}

impl Inspector {
    pub fn new(target: WindowHandle) -> Result<Inspector, String> {
        // SAFETY: IsWindow accepts any value.
        if !unsafe { IsWindow(Some(hwnd(target))) }.as_bool() {
            return Err(format!("{target} is not a window"));
        }
        Ok(Inspector {
            target: target.0 as isize,
        })
    }

    fn hwnd(&self) -> HWND {
        hwnd(WindowHandle(self.target as u64))
    }
}

impl WindowInspector for Inspector {
    fn sample(&mut self, t_ns: u64) -> WindowSample {
        let h = self.hwnd();
        // SAFETY: read-only window queries on a handle that may have died;
        // each tolerates that.
        let (exists, visible, minimized, hung) = unsafe {
            (
                IsWindow(Some(h)).as_bool(),
                IsWindowVisible(h).as_bool(),
                IsIconic(h).as_bool(),
                IsHungAppWindow(h).as_bool(),
            )
        };
        if !exists {
            return WindowSample {
                t_ns,
                foreground: Some(false),
                visible: Some(false),
                ..WindowSample::default()
            };
        }
        let (occluded_fraction, occluders) = match bounds(h) {
            Some(target) => {
                let mut above = Vec::new();
                let mut info = Vec::new();
                // SAFETY: walking the z-order upwards from our window.
                let mut w = unsafe { GetWindow(h, GW_HWNDPREV) }.unwrap_or_default();
                let mut guard = 0;
                while !w.is_invalid() && guard < 4096 {
                    guard += 1;
                    if let Some(b) = occluder_bounds(w) {
                        above.push(b);
                        info.push(w);
                    }
                    // SAFETY: as above.
                    w = unsafe { GetWindow(w, GW_HWNDPREV) }.unwrap_or_default();
                }
                let (fraction, who) = occlusion(&target, &above);
                let occ = who
                    .into_iter()
                    .map(|i| OccluderInfo {
                        class: class_name(info[i]),
                        pid: pid_of(info[i]),
                    })
                    .collect();
                (Some(fraction), occ)
            }
            None => (None, Vec::new()),
        };
        WindowSample {
            t_ns,
            foreground: Some(is_foreground(h)),
            visible: Some(visible),
            cloaked: cloaked(h),
            minimized: Some(minimized),
            occluded_fraction,
            occluders,
            hung: Some(hung),
        }
    }

    fn describe(&self) -> String {
        let h = self.hwnd();
        format!("{} {} (pid {})", handle_of(h), class_name(h), pid_of(h))
    }
}

unsafe extern "system" fn collect_windows(h: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec<isize> passed by `top_level_windows`,
    // alive for the duration of EnumWindows.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<isize>) };
    out.push(h.0 as isize);
    BOOL::from(true)
}

fn top_level_windows() -> Vec<HWND> {
    let mut v: Vec<isize> = Vec::new();
    // SAFETY: the callback only pushes into `v`, which outlives the call.
    let _ = unsafe {
        EnumWindows(
            Some(collect_windows),
            LPARAM(&mut v as *mut Vec<isize> as isize),
        )
    };
    v.into_iter()
        .map(|h| HWND(h as *mut core::ffi::c_void))
        .collect()
}

fn candidate(h: HWND) -> Option<Rect> {
    // SAFETY: read-only queries.
    unsafe {
        if !IsWindowVisible(h).as_bool() || GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return None;
        }
    }
    if cloaked(h) == Some(true) {
        return None;
    }
    bounds(h)
}

/// Resolve a target to a window handle.
pub fn resolve(spec: &TargetSpec) -> Result<WindowHandle, String> {
    match spec {
        TargetSpec::Handle(v) => {
            let h = hwnd(WindowHandle(*v));
            // SAFETY: IsWindow accepts any value.
            if unsafe { IsWindow(Some(h)) }.as_bool() {
                Ok(WindowHandle(*v))
            } else {
                Err(format!("0x{v:x} is not a window"))
            }
        }
        TargetSpec::Pid(pid) => top_level_windows()
            .into_iter()
            .filter(|&h| pid_of(h) == *pid)
            .filter_map(|h| candidate(h).map(|r| (h, r.area())))
            .max_by_key(|&(_, a)| a)
            .map(|(h, _)| handle_of(h))
            .ok_or_else(|| format!("process {pid} has no visible top-level window")),
        TargetSpec::Title(t) => top_level_windows()
            .into_iter()
            .filter(|&h| title(h).contains(t.as_str()))
            .filter_map(|h| candidate(h).map(|r| (h, r.area())))
            .max_by_key(|&(_, a)| a)
            .map(|(h, _)| handle_of(h))
            .ok_or_else(|| format!("no visible top-level window title contains {t:?}")),
    }
}

/// The refresh interval of the display the window is on: DWM's composition
/// period, else the monitor's display settings, else an assumed 60 Hz (and
/// the source says which).
pub fn refresh(target: WindowHandle) -> Refresh {
    let mut timing = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    // SAFETY: cbSize is set; the call writes one struct.
    if unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing) }.is_ok()
        && timing.qpcRefreshPeriod > 0
    {
        let ns = qpc_ticks_to_ns(timing.qpcRefreshPeriod as i64);
        let hz = 1e9 / ns as f64;
        if (20.0..=500.0).contains(&hz) {
            return Refresh {
                hz,
                ns,
                source: "DwmGetCompositionTimingInfo".into(),
            };
        }
    }
    // SAFETY: MonitorFromWindow never fails with DEFAULTTONEAREST; the info
    // and mode structs carry their sizes.
    unsafe {
        let mon = MonitorFromWindow(hwnd(target), MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(mon, &mut info.monitorInfo).as_bool() {
            let mut mode = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            if EnumDisplaySettingsW(
                windows::core::PCWSTR(info.szDevice.as_ptr()),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
            .as_bool()
                && mode.dmDisplayFrequency > 1
            {
                return Refresh::from_hz(
                    f64::from(mode.dmDisplayFrequency),
                    "EnumDisplaySettingsW",
                );
            }
        }
    }
    Refresh::from_hz(60.0, "assumed (no composition timing or display frequency)")
}

/// Make `h` the foreground window: the documented route first, then the
/// thread-input attachment route, then SwitchToThisWindow. Returns whether it
/// is foreground afterwards.
pub fn activate(h: HWND) -> bool {
    if is_foreground(h) {
        return true;
    }
    // SAFETY: window-management calls on our own (or the caller's chosen)
    // window; failures are tolerated and re-checked.
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if is_foreground(h) {
            return true;
        }
        let fg = GetForegroundWindow();
        let fg_thread = if fg.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(fg, None)
        };
        let me = GetCurrentThreadId();
        let attached =
            fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        if is_foreground(h) {
            return true;
        }
        SwitchToThisWindow(h, true);
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    is_foreground(h)
}
