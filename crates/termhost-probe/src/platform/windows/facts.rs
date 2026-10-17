//! The doctor's facts on Windows, each from a real probe.
//!
//! Capture is proven end to end: a small, non-activating, topmost probe window
//! (it never takes focus and receives no input) shows a calibration marker for
//! about a second while WGC and DXGI duplication each capture it and the
//! decoder must read the marker back.

use std::time::Duration;

use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use super::{dda, input, process, session, testwin, wgc};
use crate::codec::{Marker, MarkerKind};
use crate::doctor::{CaptureFacts, ClockFacts, Facts, GpuFacts, InputFacts};
use crate::platform::common::{foreground_finding, probe_capture, ForegroundFinding};
use crate::platform::{PaintSlot, Tagging, TestWindow, TestWindowOptions};

fn work_area_origin() -> (i32, i32) {
    let mut r = windows::Win32::Foundation::RECT::default();
    // SAFETY: SPI_GETWORKAREA writes one RECT.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_ok() {
        (r.left + 8, r.top + 8)
    } else {
        (8, 8)
    }
}

pub fn gather(node: &str) -> Facts {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "unknown-host".into());
    let os = sysinfo::System::long_os_version().unwrap_or_else(|| "Windows".into());
    let session = session::session_facts();
    let clock: ClockFacts = crate::clockcheck::node_round_trip(
        node,
        40,
        crate::clockcheck::NODE_STARTUP,
        Duration::from_secs(10),
    );
    let gpu = match process::gpu_instances() {
        Ok(n) => GpuFacts::Instances(n),
        Err(why) if why.contains("does not exist") => GpuFacts::Absent(why),
        Err(why) => GpuFacts::Failed(why),
    };
    let listener = input::start_monitor(&Tagging::new());
    let input_facts = InputFacts {
        supported: Ok(()),
        listener: listener.as_ref().map(|m| m.describe.clone()).map_err(Clone::clone),
        injector: Ok("SendInput (virtual keys) with a per-run dwExtraInfo tag; Raw Input hearing those tagged keys is proven by `selftest`, not here".into()),
    };
    drop(listener);

    let (wgc_facts, dda_facts, foreground_violation, foreground_note) = match &session.state {
        Err(problem) => {
            let why = format!("{problem:?}");
            (
                CaptureFacts::NoSession(why.clone()),
                CaptureFacts::NoSession(why),
                None,
                None,
            )
        }
        Ok(_) => {
            // D9 around and DURING the probe window: whose window was
            // foreground (`foreground_finding`). A finding is recorded,
            // never repaired (moving focus back would be a second
            // intrusion).
            let before = foreground();
            let mut during = Vec::new();
            let mut created = Vec::new();
            let (wgc, dda) = capture_facts(&mut during, &mut created);
            let after = foreground();
            match foreground_finding(before, &during, after, |h| created.contains(&h)) {
                ForegroundFinding::Clean => (wgc, dda, None, None),
                ForegroundFinding::ProbeTookIt(v) => (wgc, dda, Some(v), None),
                ForegroundFinding::ChangedNotByProbe(n) => (wgc, dda, None, Some(n)),
            }
        }
    };
    Facts {
        host,
        os,
        session,
        wgc: wgc_facts,
        dda: dda_facts,
        x11: CaptureFacts::NotApplicable("X11 capture is Linux-only; this is Windows".into()),
        input: input_facts,
        clock,
        gpu,
        foreground_violation,
        foreground_note,
    }
}

/// The foreground window's handle (0 = none).
fn foreground() -> usize {
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.0 as usize
}

/// Capture the doctor's probe window with WGC and DDA, sampling the
/// foreground into `during` right after the window is created and after
/// each capture probe, and naming the windows the probe created in
/// `created`.
fn capture_facts(
    during: &mut Vec<usize>,
    created: &mut Vec<usize>,
) -> (CaptureFacts, CaptureFacts) {
    let (x, y) = work_area_origin();
    let opts = TestWindowOptions {
        title: "termhost-probe doctor".into(),
        x,
        y,
        client_w: 0,
        client_h: 0,
        activate: false,
        topmost: true,
        cell_w: 12,
        cell_h: 20,
        echo_session: 0,
    };
    let win = match testwin::GdiTestWindow::create(&opts) {
        Ok(w) => w,
        Err(why) => {
            let f = CaptureFacts::OpenFailed(format!("could not show the probe window: {why}"));
            return (f.clone(), f);
        }
    };
    created.extend(win.created_windows());
    during.push(foreground());
    let expect = Marker::new(MarkerKind::Calibration, 0, 0);
    std::thread::sleep(Duration::from_millis(150));
    let painted = win.paint(PaintSlot::Output, Some(expect));
    if let Err(why) = painted {
        let f = CaptureFacts::OpenFailed(format!("could not paint the probe window: {why}"));
        return (f.clone(), f);
    }
    let handle = win.handle();
    let wgc_facts = match wgc::WgcSource::open(handle) {
        Ok(mut src) => {
            // Repaint after the session starts so a frame is guaranteed.
            let _ = win.paint(PaintSlot::Output, None);
            let _ = win.paint(PaintSlot::Output, Some(expect));
            probe_capture(&mut src, expect, Duration::from_millis(2000))
        }
        Err(why) if why.contains("not supported") => CaptureFacts::Unavailable(why),
        Err(why) => CaptureFacts::OpenFailed(why),
    };
    during.push(foreground());
    let dda_facts = match dda::DdaSource::open(handle) {
        Ok(mut src) => {
            let _ = win.paint(PaintSlot::Output, None);
            let _ = win.paint(PaintSlot::Output, Some(expect));
            probe_capture(&mut src, expect, Duration::from_millis(2000))
        }
        Err(why) if why.contains("UNSUPPORTED") || why.contains("E_ACCESSDENIED") => {
            CaptureFacts::Unavailable(why)
        }
        Err(why) => CaptureFacts::OpenFailed(why),
    };
    during.push(foreground());
    win.close();
    (wgc_facts, dda_facts)
}
