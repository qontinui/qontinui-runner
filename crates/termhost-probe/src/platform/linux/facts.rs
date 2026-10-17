//! The doctor's facts on Linux, each from a real probe: the X display, a
//! capture of the doctor's own override-redirect probe window through DAMAGE +
//! shm with its calibration marker decoded, the XInput2 listener, XTEST, and
//! the node clock.

use std::time::Duration;

use super::capture::X11Source;
use super::input::{start_monitor, xtest_available};
use super::testwin::X11TestWindow;
use super::x11::Display;
use crate::codec::{Marker, MarkerKind};
use crate::doctor::{
    CaptureFacts, ClockFacts, Facts, GpuFacts, InputFacts, SessionFacts, SessionProblem,
};
use crate::platform::common::probe_capture;
use crate::platform::{PaintSlot, Tagging, TestWindow, TestWindowOptions};

pub fn gather(node: &str) -> Facts {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "unknown-host".into());
    let os = sysinfo::System::long_os_version().unwrap_or_else(|| "Linux".into());
    let clock: ClockFacts = crate::clockcheck::node_round_trip(
        node,
        40,
        crate::clockcheck::NODE_STARTUP,
        Duration::from_secs(10),
    );
    let not_windows = || CaptureFacts::NotApplicable("Windows-only; this is Linux".into());
    let gpu =
        GpuFacts::NotApplicable("per-process GPU engine counters are Windows-only (PDH)".into());
    let display = match Display::open() {
        Ok(d) => d,
        Err(why) => {
            return Facts {
                host,
                os,
                session: SessionFacts {
                    supported: Ok(()),
                    state: Err(SessionProblem::NotInteractive(why.clone())),
                },
                wgc: not_windows(),
                dda: not_windows(),
                x11: CaptureFacts::NoSession(why.clone()),
                input: InputFacts {
                    supported: Ok(()),
                    listener: Err(why.clone()),
                    injector: Err(why),
                },
                clock,
                gpu,
                foreground_violation: None,
                foreground_note: None,
            }
        }
    };
    let session = SessionFacts {
        supported: Ok(()),
        state: Ok(display.describe()),
    };
    let listener = start_monitor(&Tagging::new());
    let input = InputFacts {
        supported: Ok(()),
        listener: listener
            .as_ref()
            .map(|m| m.describe.clone())
            .map_err(Clone::clone),
        injector: xtest_available(&display),
    };
    drop(listener);
    Facts {
        host,
        os,
        session,
        wgc: not_windows(),
        dda: not_windows(),
        x11: capture_facts(),
        input,
        clock,
        gpu,
        foreground_violation: None,
        foreground_note: None,
    }
}

fn capture_facts() -> CaptureFacts {
    let opts = TestWindowOptions {
        title: "termhost-probe doctor".into(),
        x: 8,
        y: 8,
        client_w: 0,
        client_h: 0,
        activate: false,
        topmost: true,
        cell_w: 12,
        cell_h: 20,
        echo_session: 0,
    };
    let win = match X11TestWindow::create(&opts) {
        Ok(w) => w,
        Err(why) => {
            return CaptureFacts::OpenFailed(format!("could not show the probe window: {why}"))
        }
    };
    let expect = Marker::new(MarkerKind::Calibration, 0, 0);
    let facts = match X11Source::open(win.handle()) {
        Ok(mut src) => {
            let _ = win.paint(PaintSlot::Output, Some(expect));
            let probed = probe_capture(&mut src, expect, Duration::from_millis(2000));
            let mut reduced: Vec<String> = Vec::new();
            if let Some(n) = src.shm_note() {
                reduced.push(format!(
                    "{n}: reads go through GetImage, copying every frame through the X socket"
                ));
            }
            if !src.clock_established() {
                reduced.push("the X server clock could not be established, so the probe's frame lag cannot be measured and the KPIs that guard depends on would be unknown(harness_error)".into());
            }
            match probed {
                CaptureFacts::Frame {
                    decoded, detail, ..
                } => CaptureFacts::Frame {
                    decoded,
                    degraded: (!reduced.is_empty()).then(|| reduced.join("; ")),
                    detail,
                },
                other => other,
            }
        }
        Err(why) if why.contains("DAMAGE") || why.contains("TrueColor") => {
            CaptureFacts::Unavailable(why)
        }
        Err(why) => CaptureFacts::OpenFailed(why),
    };
    win.close();
    facts
}
