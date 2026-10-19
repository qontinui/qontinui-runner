//! Platforms with neither Windows capture nor X11 (e.g. macOS): every
//! mechanism answers that it is unavailable here, which the doctor reports as
//! INOPERATIVE-ON-THIS-MACHINE, and `watch` / `inject` / `selftest` refuse with
//! that reason instead of measuring nothing silently.

use std::sync::Arc;
use std::time::Duration;

use super::{
    CaptureSource, GpuReading, Host, InputMonitor, KeyInjector, ProcMem, ProcessReader, Refresh,
    Tagging, TargetSpec, TestWindow, TestWindowOptions, WindowHandle, WindowInspector,
};
use crate::doctor::{CaptureFacts, Facts, GpuFacts, InputFacts, SessionFacts};
use crate::records::SourceId;

const WHY: &str = "termhost-probe supports Windows and Linux/X11 only";

pub fn init_process() {}

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

struct NoProcesses;

impl ProcessReader for NoProcesses {
    fn cpu_ns(&mut self, _pid: u32) -> Option<u64> {
        None
    }
    fn memory(&mut self, _pid: u32) -> Option<ProcMem> {
        None
    }
    fn gpu(&mut self) -> GpuReading {
        GpuReading::Unsupported(WHY.into())
    }
}

impl Host for RealHost {
    fn doctor_facts(&mut self, node: &str) -> Facts {
        Facts {
            host: sysinfo::System::host_name().unwrap_or_else(|| "unknown-host".into()),
            os: std::env::consts::OS.into(),
            session: SessionFacts {
                supported: Err(WHY.into()),
                state: Err(crate::doctor::SessionProblem::NotInteractive(WHY.into())),
            },
            wgc: CaptureFacts::NotApplicable(WHY.into()),
            dda: CaptureFacts::NotApplicable(WHY.into()),
            x11: CaptureFacts::NotApplicable(WHY.into()),
            input: InputFacts {
                supported: Err(WHY.into()),
                listener: Err(WHY.into()),
                injector: Err(WHY.into()),
            },
            clock: crate::clockcheck::node_round_trip(
                node,
                40,
                crate::clockcheck::NODE_STARTUP,
                Duration::from_secs(10),
            ),
            gpu: GpuFacts::NotApplicable(WHY.into()),
            foreground_violation: None,
            foreground_note: None,
        }
    }
    fn source_kinds(&self) -> Vec<SourceId> {
        Vec::new()
    }
    fn open_source(
        &mut self,
        _kind: SourceId,
        _target: WindowHandle,
    ) -> Result<Box<dyn CaptureSource>, String> {
        Err(WHY.into())
    }
    fn inspector(&mut self, _target: WindowHandle) -> Result<Box<dyn WindowInspector>, String> {
        Err(WHY.into())
    }
    fn input_monitor(&mut self, _tagging: &Tagging) -> Result<InputMonitor, String> {
        Err(WHY.into())
    }
    fn injector(
        &mut self,
        _target: WindowHandle,
        _tagging: &Tagging,
    ) -> Result<Box<dyn KeyInjector>, String> {
        Err(WHY.into())
    }
    fn create_test_window(
        &mut self,
        _opts: &TestWindowOptions,
    ) -> Result<Arc<dyn TestWindow>, String> {
        Err(WHY.into())
    }
    fn refresh(&mut self, _target: WindowHandle) -> Refresh {
        Refresh::assumed_60("unsupported platform")
    }
    fn process_reader(&mut self) -> Box<dyn ProcessReader> {
        Box::new(NoProcesses)
    }
    fn reports_hung_windows(&self) -> bool {
        false
    }
    fn resolve_target(&mut self, _spec: &TargetSpec) -> Result<WindowHandle, String> {
        Err(WHY.into())
    }
}
