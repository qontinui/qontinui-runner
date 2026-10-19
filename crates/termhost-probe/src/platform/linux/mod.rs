//! Linux / X11: damage + shm capture (D2), XInput2 raw events (the real-input
//! listener), XTEST injection, an X11 window for the selftest and the doctor's
//! probe, and /proc accounting. Pure-Rust X11 (x11rb's `RustConnection`): no
//! libxcb and no X11 headers.

mod capture;
mod facts;
mod input;
mod process;
mod testwin;
mod x11;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::{
    CaptureSource, Host, InputMonitor, KeyInjector, ProcessReader, Refresh, Tagging, TargetSpec,
    TestWindow, TestWindowOptions, WindowHandle, WindowInspector, STOP,
};
use crate::doctor::Facts;
use crate::records::SourceId;

extern "C" fn on_signal(_: libc::c_int) {
    // An atomic store is async-signal-safe.
    STOP.store(true, Ordering::Relaxed);
}

/// Process-wide setup: SIGINT / SIGTERM end `watch` with its summary written.
pub fn init_process() {
    // SAFETY: installing a handler that only performs an atomic store.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

/// The real Linux platform.
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
        vec![SourceId::X11]
    }

    fn open_source(
        &mut self,
        kind: SourceId,
        target: WindowHandle,
    ) -> Result<Box<dyn CaptureSource>, String> {
        match kind {
            SourceId::X11 => Ok(Box::new(capture::X11Source::open(target)?)),
            other => Err(format!("{} capture is Windows-only", other.as_str())),
        }
    }

    fn inspector(&mut self, target: WindowHandle) -> Result<Box<dyn WindowInspector>, String> {
        Ok(Box::new(x11::Inspector::new(target)?))
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
        Ok(Arc::new(testwin::X11TestWindow::create(opts)?))
    }

    fn refresh(&mut self, _target: WindowHandle) -> Refresh {
        x11::refresh()
    }

    fn process_reader(&mut self) -> Box<dyn ProcessReader> {
        Box::new(process::ProcReader::new())
    }

    fn reports_hung_windows(&self) -> bool {
        // X11 has no OS-level "not responding" state (that is a window
        // manager's _NET_WM_PING, absent on a bare Xvfb).
        false
    }

    fn resolve_target(&mut self, spec: &TargetSpec) -> Result<WindowHandle, String> {
        x11::resolve(spec)
    }
}
