//! An in-memory desktop implementing the platform seam: one window whose
//! pixels are a synthetic canvas, a compositor that presents changes on a
//! 60 Hz vsync grid (like DWM: a frame only when something changed), a window
//! manager that reports focus and occlusion, a real-input bus, and an
//! injector whose keys reach the window and echo. Faults can be switched on to
//! prove each selftest gate can fail.
//!
//! The compositor is deterministic: a change is presented at the first vsync
//! at or after it (after a freeze ends, while the window is frozen), and the
//! frame carries that vsync's time however late the capture thread wakes. On a
//! loaded CI box the thread's wake-up jitter would otherwise land in every
//! latency the fake reports, and a gate's pass would depend on the scheduler.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use termhost_probe::clock::now_ns;
use termhost_probe::codec::{encode_rgb, marker_seq, Marker, MarkerKind, CELLS};
use termhost_probe::doctor::{
    alignment, CaptureFacts, ClockFacts, Facts, GpuFacts, InputFacts, SessionFacts,
};
use termhost_probe::model::{InputDevice, InputObs, InputOrigin, WindowSample};
use termhost_probe::platform::{
    CaptureSource, CaptureStatus, EchoPainted, Host, InjectBlock, InputBus, InputMonitor,
    KeyInjector, PaintSlot, ProcessReader, RawFrame, Refresh, Tagging, TargetSpec, TestGeometry,
    TestWindow, TestWindowOptions, WindowHandle, WindowInspector,
};
use termhost_probe::raster::synth::Canvas;
use termhost_probe::records::SourceId;

pub const REFRESH_NS: u64 = 16_666_667;
const WIN: WindowHandle = WindowHandle(0x5150);

/// Switchable faults.
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    /// Every presented frame is stamped this much later than its vsync.
    pub capture_lag_ms: u64,
    /// The listener never hears injections that lack the run's tag.
    pub deaf_to_foreign: bool,
    /// The window manager never reports occlusion.
    pub occlusion_blind: bool,
    /// Every third key never reaches the window.
    pub drop_keys: bool,
    /// The compositor never presents a frame.
    pub capture_dead: bool,
    /// ONE frame (the fifth) reaches the probe this long after it was
    /// presented — a scheduler hiccup, not a probe that fell behind.
    pub one_late_frame_ms: u64,
    /// Every frame reaches the probe this long after it was presented (the
    /// probe is behind its source).
    pub probe_backlog_ms: u64,
    /// The source cannot say when its frames were presented (an X server
    /// clock the probe could not establish).
    pub presentation_unknown: bool,
}

struct State {
    foreground: bool,
    occluded: bool,
    frozen_until: u64,
    echoes: Vec<EchoPainted>,
    keys_seen: u32,
}

pub struct Desktop {
    canvas: Mutex<Canvas>,
    generation: AtomicU64,
    /// When the canvas last changed (stored before `generation` moves).
    last_change_ns: AtomicU64,
    state: Mutex<State>,
    bus: Mutex<Option<Arc<InputBus>>>,
    geometry: TestGeometry,
    faults: Faults,
}

impl Desktop {
    pub fn new(faults: Faults) -> Arc<Desktop> {
        Arc::new(Desktop {
            canvas: Mutex::new(Canvas::new(
                8 + 12 * CELLS + 8,
                8 + 24 + 8 + 24 + 8,
                [0, 0, 0],
            )),
            generation: AtomicU64::new(1),
            last_change_ns: AtomicU64::new(0),
            state: Mutex::new(State {
                foreground: false,
                occluded: false,
                frozen_until: 0,
                echoes: Vec::new(),
                keys_seen: 0,
            }),
            bus: Mutex::new(None),
            geometry: TestGeometry {
                cell_w: 12,
                cell_h: 24,
                output_at: (8, 8),
                echo_at: (8, 40),
            },
            faults,
        })
    }

    fn draw(&self, slot: PaintSlot, marker: Option<Marker>) {
        let (x, y) = match slot {
            PaintSlot::Output => self.geometry.output_at,
            PaintSlot::Echo => self.geometry.echo_at,
        };
        let colours = marker.map(encode_rgb).unwrap_or([[0, 0, 0]; CELLS]);
        let mut c = self.canvas.lock().expect("canvas");
        for (i, rgb) in colours.iter().enumerate() {
            c.fill_rect(x as usize + i * 12, y as usize, 12, 24, *rgb);
        }
        self.last_change_ns.store(now_ns(), Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn publish(&self, origin: InputOrigin) {
        if let Some(bus) = self.bus.lock().expect("bus").as_ref() {
            bus.publish(InputObs {
                t_ns: now_ns(),
                device: InputDevice::Keyboard,
                origin,
            });
        }
    }

    /// An input event, as the desktop's listener would report it (a test's
    /// "operator").
    pub fn publish_input(&self, origin: InputOrigin) {
        self.publish(origin);
    }

    /// Keys the window has received.
    pub fn keys_seen(&self) -> u32 {
        self.state.lock().expect("state").keys_seen
    }
}

/// The compositor: presents the window on the vsync grid when it changed.
/// A frame's time is its vsync, and it counts as presented when the fake
/// delivers it — so the capture thread's wake-up jitter lands in neither the
/// latencies nor the probe's lag, unless a fault says otherwise.
struct FakeCapture {
    id: SourceId,
    desk: Arc<Desktop>,
    last: u64,
    delivered: u64,
}

impl CaptureSource for FakeCapture {
    fn id(&self) -> SourceId {
        self.id
    }
    fn describe(&self) -> String {
        "fake compositor".into()
    }
    fn next_frame(&mut self, timeout: Duration, f: &mut dyn FnMut(RawFrame<'_>)) -> CaptureStatus {
        let deadline = now_ns() + timeout.as_nanos() as u64;
        loop {
            let generation = self.desk.generation.load(Ordering::SeqCst);
            if generation != self.last && !self.desk.faults.capture_dead {
                let changed = self.desk.last_change_ns.load(Ordering::SeqCst);
                let frozen_until = self.desk.state.lock().expect("state").frozen_until;
                let present_at = changed.max(frozen_until).div_ceil(REFRESH_NS) * REFRESH_NS;
                if now_ns() >= present_at {
                    self.last = generation;
                    let c = self.desk.canvas.lock().expect("canvas");
                    let view = c.view();
                    let faults = self.desk.faults;
                    self.delivered += 1;
                    let late = if self.delivered == 5 {
                        faults.one_late_frame_ms
                    } else {
                        faults.probe_backlog_ms
                    };
                    let presented_ns = (!faults.presentation_unknown)
                        .then(|| now_ns().saturating_sub(late * 1_000_000));
                    f(RawFrame {
                        t_ns: present_at + faults.capture_lag_ms * 1_000_000,
                        presented_ns,
                        view,
                        origin: (0, 0),
                        missed: None,
                        window_size: (c.width() as u32, c.height() as u32),
                    });
                    return CaptureStatus::Frame;
                }
            }
            if now_ns() >= deadline {
                return CaptureStatus::Timeout;
            }
            // A plain sleep, not `sleep_until_ns`: that spins its last 2 ms,
            // and parallel tests would each pin a core polling.
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct FakeInspector {
    desk: Arc<Desktop>,
}

impl WindowInspector for FakeInspector {
    fn sample(&mut self, t_ns: u64) -> WindowSample {
        let st = self.desk.state.lock().expect("state");
        let occluded = st.occluded && !self.desk.faults.occlusion_blind;
        WindowSample {
            t_ns,
            foreground: Some(st.foreground),
            visible: Some(true),
            cloaked: Some(false),
            minimized: Some(false),
            occluded_fraction: Some(if occluded { 1.0 } else { 0.0 }),
            occluders: vec![],
            hung: Some(st.frozen_until > t_ns),
        }
    }
    fn describe(&self) -> String {
        "fake window manager".into()
    }
}

struct FakeInjector {
    desk: Arc<Desktop>,
    tagging: Tagging,
    activations: Arc<AtomicU64>,
}

impl KeyInjector for FakeInjector {
    fn check_target(&mut self) -> Result<(), InjectBlock> {
        if self.desk.state.lock().expect("state").foreground {
            Ok(())
        } else {
            Err(InjectBlock::NotForeground("fake: another window".into()))
        }
    }
    fn can_send(&self, ch: char) -> bool {
        ch.is_ascii_lowercase() || ch.is_ascii_digit()
    }
    fn ledger_ids(&self, _ch: char) -> Vec<u32> {
        Vec::new()
    }
    fn heard(&self) -> u64 {
        self.tagging.recognised.load(Ordering::Relaxed)
    }
    fn send_char(&mut self, ch: char, tagged: bool) -> Result<(), String> {
        // The listener hears both events of the key press.
        for _ in 0..2 {
            if tagged {
                self.tagging.recognised.fetch_add(1, Ordering::Relaxed);
                self.desk.publish(InputOrigin::Probe);
            } else if !self.desk.faults.deaf_to_foreign {
                self.desk.publish(InputOrigin::ForeignInjected);
            }
        }
        if tagged {
            self.tagging.sent.fetch_add(2, Ordering::Relaxed);
        }
        // The window receives the key and echoes it.
        let seq = {
            let mut st = self.desk.state.lock().expect("state");
            st.keys_seen += 1;
            if self.desk.faults.drop_keys && st.keys_seen.is_multiple_of(3) {
                return Ok(());
            }
            st.echoes.len() as u32
        };
        let m = Marker::new(
            MarkerKind::Echo,
            termhost_probe::selftest::SESSION,
            marker_seq(u64::from(seq)),
        );
        self.desk.draw(PaintSlot::Echo, Some(m));
        self.desk
            .state
            .lock()
            .expect("state")
            .echoes
            .push(EchoPainted {
                seq,
                ch,
                t_painted_ns: now_ns(),
            });
        Ok(())
    }
    fn activate(&mut self) -> Result<(), String> {
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.desk.state.lock().expect("state").foreground = true;
        Ok(())
    }
    fn describe(&self) -> String {
        "fake injector".into()
    }
}

struct FakeWindow {
    desk: Arc<Desktop>,
}

impl TestWindow for FakeWindow {
    fn handle(&self) -> WindowHandle {
        WIN
    }
    fn geometry(&self) -> TestGeometry {
        self.desk.geometry
    }
    fn paint(&self, slot: PaintSlot, marker: Option<Marker>) -> Result<u64, String> {
        self.desk.draw(slot, marker);
        Ok(now_ns())
    }
    fn set_occluded(&self, on: bool) -> Result<(), String> {
        self.desk.state.lock().expect("state").occluded = on;
        Ok(())
    }
    fn activate(&self) -> Result<bool, String> {
        self.desk.state.lock().expect("state").foreground = true;
        Ok(true)
    }
    fn freeze(&self, d: Duration) -> Result<(), String> {
        self.desk.state.lock().expect("state").frozen_until = now_ns() + d.as_nanos() as u64;
        Ok(())
    }
    fn echoes(&self) -> Vec<EchoPainted> {
        self.desk.state.lock().expect("state").echoes.clone()
    }
    fn close(&self) {}
}

struct OwnProcess;

impl ProcessReader for OwnProcess {
    fn cpu_ns(&mut self, _pid: u32) -> Option<u64> {
        Some(now_ns() / 4)
    }
    fn memory(&mut self, _pid: u32) -> Option<termhost_probe::platform::ProcMem> {
        Some(termhost_probe::platform::ProcMem {
            catalog_bytes: 50 << 20,
            rss_bytes: 80 << 20,
        })
    }
    fn gpu(&mut self) -> termhost_probe::platform::GpuReading {
        termhost_probe::platform::GpuReading::Unsupported("fake".into())
    }
}

pub struct FakeHost {
    pub desk: Arc<Desktop>,
    /// What the doctor's WGC proof reports (a decoded frame by default).
    pub wgc_facts: CaptureFacts,
    /// What the doctor's GPU counter probe reports (absent by default).
    pub gpu_facts: GpuFacts,
    /// A second source, Desktop Duplication, sees the same desktop.
    pub dda: bool,
    /// The display reports no refresh rate (60 Hz is assumed).
    pub refresh_assumed: bool,
    /// Times `activate` was called on the injector.
    pub activations: Arc<AtomicU64>,
}

fn decoded() -> CaptureFacts {
    CaptureFacts::Frame {
        decoded: Ok(Marker::new(MarkerKind::Calibration, 0, 0)),
        degraded: None,
        detail: "fake".into(),
    }
}

impl FakeHost {
    pub fn new(faults: Faults) -> FakeHost {
        FakeHost {
            desk: Desktop::new(faults),
            wgc_facts: decoded(),
            gpu_facts: GpuFacts::Absent("fake".into()),
            dda: false,
            refresh_assumed: false,
            activations: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Host for FakeHost {
    fn doctor_facts(&mut self, _node: &str) -> Facts {
        Facts {
            host: "fake".into(),
            os: "fake".into(),
            session: SessionFacts {
                supported: Ok(()),
                state: Ok("fake interactive desktop".into()),
            },
            wgc: self.wgc_facts.clone(),
            dda: if self.dda {
                decoded()
            } else {
                CaptureFacts::NotApplicable("fake desktop has one source".into())
            },
            x11: CaptureFacts::NotApplicable("fake".into()),
            input: InputFacts {
                supported: Ok(()),
                listener: Ok("fake listener".into()),
                injector: Ok("fake injector".into()),
            },
            clock: ClockFacts::Measured(alignment(1_000, 50_000, 30)),
            gpu: self.gpu_facts.clone(),
        }
    }
    fn source_kinds(&self) -> Vec<SourceId> {
        if self.dda {
            vec![SourceId::Wgc, SourceId::Dda]
        } else {
            vec![SourceId::Wgc]
        }
    }
    fn open_source(
        &mut self,
        kind: SourceId,
        _target: WindowHandle,
    ) -> Result<Box<dyn CaptureSource>, String> {
        Ok(Box::new(FakeCapture {
            id: kind,
            desk: self.desk.clone(),
            last: 0,
            delivered: 0,
        }))
    }
    fn inspector(&mut self, _target: WindowHandle) -> Result<Box<dyn WindowInspector>, String> {
        Ok(Box::new(FakeInspector {
            desk: self.desk.clone(),
        }))
    }
    fn input_monitor(&mut self, _tagging: &Tagging) -> Result<InputMonitor, String> {
        let bus = InputBus::new();
        *self.desk.bus.lock().expect("bus") = Some(bus.clone());
        Ok(InputMonitor {
            bus,
            describe: "fake listener".into(),
            guard: Box::new(()),
        })
    }
    fn injector(
        &mut self,
        _target: WindowHandle,
        tagging: &Tagging,
    ) -> Result<Box<dyn KeyInjector>, String> {
        Ok(Box::new(FakeInjector {
            desk: self.desk.clone(),
            tagging: tagging.clone(),
            activations: self.activations.clone(),
        }))
    }
    fn create_test_window(
        &mut self,
        _opts: &TestWindowOptions,
    ) -> Result<Arc<dyn TestWindow>, String> {
        Ok(Arc::new(FakeWindow {
            desk: self.desk.clone(),
        }))
    }
    fn refresh(&mut self, _target: WindowHandle) -> Refresh {
        Refresh {
            hz: 60.0,
            ns: REFRESH_NS,
            source: "fake".into(),
            assumed: self.refresh_assumed,
        }
    }
    fn process_reader(&mut self) -> Box<dyn ProcessReader> {
        Box::new(OwnProcess)
    }
    fn reports_hung_windows(&self) -> bool {
        true
    }
    fn resolve_target(&mut self, _spec: &TargetSpec) -> Result<WindowHandle, String> {
        Ok(WIN)
    }
}
