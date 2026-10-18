//! Windows: Windows.Graphics.Capture per window (the primary source), DXGI
//! Desktop Duplication (the missed-frame cross-check), Raw Input (the real-input
//! listener — never a low-level hook), SendInput with a per-run tag, PDH GPU
//! engine counters, and a GDI window for the selftest and the doctor's probe.

mod d3d;
mod dda;
mod facts;
mod input;
mod process;
mod session;
mod testwin;
mod wgc;
mod window;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use windows::core::BOOL;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::DisableProcessWindowsGhosting;

use super::{
    CaptureSource, Host, InputMonitor, KeyInjector, ProcessReader, Refresh, Tagging, TargetSpec,
    TestWindow, TestWindowOptions, WindowHandle, WindowInspector, STOP,
};
use crate::doctor::Facts;
use crate::records::SourceId;

pub(crate) fn hwnd(h: WindowHandle) -> HWND {
    HWND(h.0 as usize as *mut core::ffi::c_void)
}

pub(crate) fn handle_of(h: HWND) -> WindowHandle {
    WindowHandle(h.0 as usize as u64)
}

unsafe extern "system" fn ctrl_handler(ctrl: u32) -> BOOL {
    STOP.store(true, Ordering::Relaxed);
    BOOL::from(matches!(
        ctrl,
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT
    ))
}

/// Process-wide setup, once, before any window work.
pub fn init_process() {
    // SAFETY: plain process-wide setting calls with constant arguments; each
    // one's failure (already set, older Windows) leaves a usable default.
    unsafe {
        // Physical pixels everywhere, so window rectangles agree with DXGI
        // desktop coordinates and the selftest paints crisp cells.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // The OS-hung proof freezes a window of ours; a ghost window must not
        // replace it (or occlude it) while it is "not responding".
        DisableProcessWindowsGhosting();
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

/// The real Windows platform.
pub struct RealHost;

impl RealHost {
    pub fn new() -> RealHost {
        RealHost
    }
}

impl Default for RealHost {
    fn default() -> Self {
        RealHost::new()
    }
}

impl Host for RealHost {
    fn doctor_facts(&mut self, node: &str) -> Facts {
        facts::gather(node)
    }

    fn source_kinds(&self) -> Vec<SourceId> {
        vec![SourceId::Wgc, SourceId::Dda]
    }

    fn open_source(
        &mut self,
        kind: SourceId,
        target: WindowHandle,
    ) -> Result<Box<dyn CaptureSource>, String> {
        match kind {
            SourceId::Wgc => Ok(Box::new(wgc::WgcSource::open(target)?)),
            SourceId::Dda => Ok(Box::new(dda::DdaSource::open(target)?)),
            other => Err(format!(
                "{} capture is not available on Windows",
                other.as_str()
            )),
        }
    }

    fn inspector(&mut self, target: WindowHandle) -> Result<Box<dyn WindowInspector>, String> {
        Ok(Box::new(window::Inspector::new(target)?))
    }

    fn input_monitor(&mut self, tagging: &Tagging) -> Result<InputMonitor, String> {
        input::start_monitor(tagging)
    }

    fn injector(
        &mut self,
        target: WindowHandle,
        tagging: &Tagging,
    ) -> Result<Box<dyn KeyInjector>, String> {
        Ok(Box::new(input::Injector::new(target, tagging.clone())?))
    }

    fn create_test_window(
        &mut self,
        opts: &TestWindowOptions,
    ) -> Result<Arc<dyn TestWindow>, String> {
        Ok(Arc::new(testwin::GdiTestWindow::create(opts)?))
    }

    fn refresh(&mut self, target: WindowHandle) -> Refresh {
        window::refresh(target)
    }

    fn process_reader(&mut self) -> Box<dyn ProcessReader> {
        Box::new(process::WinProcessReader::new())
    }

    fn reports_hung_windows(&self) -> bool {
        true
    }

    fn resolve_target(&mut self, spec: &TargetSpec) -> Result<WindowHandle, String> {
        window::resolve(spec)
    }
}

/// A Windows error as the doctor and capture records report it: the HRESULT
/// in hex, then the system's message — so an `OpenFailed` reason names the
/// code a hosted run hit, not only its prose.
pub(crate) fn hr(e: &windows::core::Error) -> String {
    format!(
        "HRESULT 0x{:08X} ({})",
        e.code().0 as u32,
        e.message().trim()
    )
}

#[cfg(test)]
mod tests {
    use windows::core::{Error, HRESULT};

    #[test]
    fn a_windows_error_names_its_hresult() {
        let text = super::hr(&Error::from(HRESULT(0x8007_0005_u32 as i32)));
        assert!(text.starts_with("HRESULT 0x80070005 ("), "{text}");
    }
}
